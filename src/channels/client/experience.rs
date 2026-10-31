//! 客户端经验总结与经验库管理。
//!
//! 草稿素材取自本通道持久化的会话消息与工具观测，不依赖运行时短期记忆，
//! 因此对话被压缩或实例重启后仍可总结。经验库读写在本通道直接完成，
//! 只有起草需要经运行时请经验管理员撰写。

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Client, ClientError, server::token, store::Session};
use crate::contracts::SharedExperienceLibrary;
use crate::domain::{
    ChannelId, ExperienceDraftContent, ExperienceEntry, ExperienceError, ExternalInput,
    FrontendKind, is_valid_experience_id,
};

/// 交给经验管理员的会话素材上限（字符）。
const TRANSCRIPT_BUDGET: usize = 60_000;
/// 超出预算时保留开头的比例：开头通常是用户目标。
const TRANSCRIPT_HEAD_SHARE: usize = 5;
const MESSAGE_CHARS: usize = 3_000;
const MAX_NOTE_CHARS: usize = 1_000;
/// 每个会话同时保留的草稿上限。
const MAX_SESSION_DRAFTS: usize = 8;

fn library(client: &Client) -> Result<&SharedExperienceLibrary, ClientError> {
    client.experience.as_ref().ok_or(ClientError::NotFound)
}

fn storage<E: std::fmt::Display>(error: E) -> ClientError {
    ClientError::Io(std::io::Error::other(error.to_string()))
}

fn invalid(error: ExperienceError) -> ClientError {
    ClientError::Invalid(match error {
        ExperienceError::InvalidId => "invalid experience id",
        ExperienceError::EmptyTitle => "experience title is empty",
        ExperienceError::TitleTooLong => "experience title is too long",
        ExperienceError::EmptyBody => "experience body is empty",
        ExperienceError::BodyTooLarge => "experience body is too large",
        ExperienceError::Malformed(_) => "malformed experience",
    })
}

fn head(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("…[截断]");
    out
}

/// 保留首尾：报错信息通常在输出末尾。
fn head_tail(text: &str, head_chars: usize, tail_chars: usize) -> String {
    let count = text.chars().count();
    if count <= head_chars + tail_chars {
        return text.to_string();
    }
    let start: String = text.chars().take(head_chars).collect();
    let end: String = text.chars().skip(count - tail_chars).collect();
    format!(
        "{start}\n…[中间省略 {} 字]…\n{end}",
        count - head_chars - tail_chars
    )
}

/// 按事件序号还原会话的文字与工具调用过程，供经验管理员撰写草稿。
pub(super) fn transcript(session: &Session) -> String {
    let mut items: Vec<(u64, String)> = Vec::new();
    for message in &session.messages {
        let content = message["content"].as_str().unwrap_or_default().trim();
        if content.is_empty() {
            continue;
        }
        let role = match message["role"].as_str() {
            Some("user") => "用户",
            Some("agent") => "Agent",
            _ => "系统",
        };
        items.push((
            message["sequence"].as_u64().unwrap_or(0),
            format!("[{role}] {}", head(content, MESSAGE_CHARS)),
        ));
    }
    for observation in &session.observations {
        let tool = observation["tool_name"].as_str().unwrap_or("tool");
        let line = match observation["kind"].as_str() {
            Some("tool_started") => format!(
                "[调用 {tool}] {}",
                head(
                    observation["input_summary"].as_str().unwrap_or_default(),
                    MESSAGE_CHARS
                )
            ),
            Some("tool_completed") => {
                let success = observation["success"].as_bool().unwrap_or(true);
                let output = observation["output"].as_str().unwrap_or_default();
                let (label, output) = if success {
                    ("结果", head_tail(output, 300, 300))
                } else {
                    ("失败", head_tail(output, 500, 1500))
                };
                format!("[{label} {tool}] {output}")
            }
            _ => continue,
        };
        items.push((observation["sequence"].as_u64().unwrap_or(0), line));
    }
    items.sort_by_key(|(sequence, _)| *sequence);
    let lines: Vec<String> = items.into_iter().map(|(_, line)| line).collect();

    let mut text = fit_budget(&lines, TRANSCRIPT_BUDGET);
    if session.history_truncated || session.observations_truncated {
        text.insert_str(0, "（会话较早的记录已被截断）\n");
    }
    text
}

fn fit_budget(lines: &[String], budget: usize) -> String {
    let total: usize = lines.iter().map(|l| l.chars().count() + 1).sum();
    if total <= budget {
        return lines.join("\n");
    }
    let head_budget = budget / TRANSCRIPT_HEAD_SHARE;
    let mut used = 0;
    let mut head_end = 0;
    for line in lines {
        let cost = line.chars().count() + 1;
        if used + cost > head_budget {
            break;
        }
        used += cost;
        head_end += 1;
    }
    let mut tail_start = lines.len();
    for line in lines[head_end..].iter().rev() {
        let cost = line.chars().count() + 1;
        if used + cost > budget {
            break;
        }
        used += cost;
        tail_start -= 1;
    }
    let mut out = lines[..head_end].to_vec();
    out.push(format!(
        "…（中间省略 {} 条记录）…",
        tail_start.saturating_sub(head_end)
    ));
    out.extend_from_slice(&lines[tail_start..]);
    out.join("\n")
}

/// 运行时起草结果转为客户端事件；合并目标附上标题便于确认。
pub(super) fn draft_event(
    client: &Client,
    draft_id: Uuid,
    outcome: &Result<ExperienceDraftContent, String>,
) -> Value {
    match outcome {
        Ok(draft) => {
            let merge_target = draft.merge_target.as_deref().and_then(|id| {
                client
                    .experience
                    .as_ref()
                    .and_then(|library| library.get(id).ok().flatten())
                    .map(|entry| json!({"id": entry.id, "title": entry.title}))
            });
            json!({"draft_id": draft_id, "status": "ready", "title": draft.title,
                "body": draft.body, "merge_target": merge_target, "error": null})
        }
        Err(error) => json!({"draft_id": draft_id, "status": "failed", "error": error}),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DraftStart {
    draft_id: Uuid,
    #[serde(default)]
    note: Option<String>,
}

/// 发起起草：记录“生成中”草稿，并把会话素材交给运行时。
pub(super) async fn start_draft(
    State(client): State<Client>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
    Json(request): Json<DraftStart>,
) -> Result<(StatusCode, Json<Value>), ClientError> {
    library(&client)?;
    let note = request
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(ClientError::Invalid("note is too long"));
    }
    let draft_key = request.draft_id.to_string();
    let mut store = client.store.lock().expect("client state mutex poisoned");
    let device = store.authenticate(token(&headers)?, Utc::now().timestamp())?;
    let session = store.session(&device.user_id, session_id)?;
    if let Some(existing) = session.experience_drafts.get(&draft_key) {
        return Ok((StatusCode::OK, Json(existing.clone())));
    }
    if session
        .experience_drafts
        .values()
        .any(|d| d["status"] == "generating")
    {
        return Err(ClientError::Conflict(
            "an experience draft is already being generated",
        ));
    }
    if session.experience_drafts.len() >= MAX_SESSION_DRAFTS {
        return Err(ClientError::Capacity);
    }
    let material = transcript(session);
    if material.trim().is_empty() {
        return Err(ClientError::Invalid("session has no history to summarize"));
    }
    let pending = json!({"draft_id": request.draft_id, "status": "generating", "note": note});
    store.append(
        &device.user_id,
        session_id,
        "experience_draft",
        pending.clone(),
    )?;
    let channel = ChannelId {
        frontend: FrontendKind::Client,
        user_id: device.user_id.clone(),
        thread_id: Some(session_id.to_string()),
    };
    let sent = client.input.try_send(ExternalInput::ExperienceDraft {
        channel,
        draft_id: request.draft_id,
        transcript: material,
        note,
    });
    if sent.is_err() {
        store.append(
            &device.user_id,
            session_id,
            "experience_draft",
            json!({"draft_id": request.draft_id, "status": "failed", "error": "运行时输入不可用，请稍后重试"}),
        )?;
    }
    drop(store);
    client.wake();
    Ok((StatusCode::ACCEPTED, Json(pending)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DraftSave {
    title: String,
    body: String,
    /// 合并到已有经验时为其 id；否则新建。
    #[serde(default)]
    target_id: Option<String>,
}

/// 保存草稿：新建或合并到已有经验，并移除会话中的草稿卡片。
pub(super) async fn save_draft(
    State(client): State<Client>,
    headers: HeaderMap,
    Path((session_id, draft_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<DraftSave>,
) -> Result<Json<Value>, ClientError> {
    let library = library(&client)?.clone();
    let mut store = client.store.lock().expect("client state mutex poisoned");
    let device = store.authenticate(token(&headers)?, Utc::now().timestamp())?;
    let draft = store
        .session(&device.user_id, session_id)?
        .experience_drafts
        .get(&draft_id.to_string())
        .ok_or(ClientError::NotFound)?;
    if draft["status"] != "ready" {
        return Err(ClientError::Conflict("experience draft is not ready"));
    }
    let now = Utc::now();
    let entry = match request.target_id.as_deref() {
        Some(id) => {
            if !is_valid_experience_id(id) {
                return Err(ClientError::NotFound);
            }
            let mut entry = library
                .get(id)
                .map_err(storage)?
                .ok_or(ClientError::NotFound)?;
            entry
                .revise(&request.title, &request.body, now)
                .map_err(invalid)?;
            entry
        }
        None => ExperienceEntry::new(
            &request.title,
            &request.body,
            Some(session_id.to_string()),
            now,
        )
        .map_err(invalid)?,
    };
    library.save(&entry).map_err(storage)?;
    store.append(
        &device.user_id,
        session_id,
        "experience_draft",
        json!({"draft_id": draft_id, "status": "saved", "experience_id": entry.id}),
    )?;
    drop(store);
    tracing::info!(event = "ExperienceSaved", experience_id = %entry.id, merged = request.target_id.is_some(), "经验已保存");
    client.wake();
    Ok(Json(json!(entry.summary())))
}

pub(super) async fn discard_draft(
    State(client): State<Client>,
    headers: HeaderMap,
    Path((session_id, draft_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ClientError> {
    let mut store = client.store.lock().expect("client state mutex poisoned");
    let device = store.authenticate(token(&headers)?, Utc::now().timestamp())?;
    if !store
        .session(&device.user_id, session_id)?
        .experience_drafts
        .contains_key(&draft_id.to_string())
    {
        return Err(ClientError::NotFound);
    }
    store.append(
        &device.user_id,
        session_id,
        "experience_draft",
        json!({"draft_id": draft_id, "status": "discarded"}),
    )?;
    drop(store);
    client.wake();
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn list(
    State(client): State<Client>,
    headers: HeaderMap,
) -> Result<Json<Value>, ClientError> {
    super::server::authenticate(&client, &headers)?;
    let entries = library(&client)?.list().map_err(storage)?;
    let summaries: Vec<_> = entries.iter().map(ExperienceEntry::summary).collect();
    Ok(Json(json!({"experiences": summaries})))
}

fn load(client: &Client, id: &str) -> Result<ExperienceEntry, ClientError> {
    if !is_valid_experience_id(id) {
        return Err(ClientError::NotFound);
    }
    library(client)?
        .get(id)
        .map_err(storage)?
        .ok_or(ClientError::NotFound)
}

pub(super) async fn get(
    State(client): State<Client>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ExperienceEntry>, ClientError> {
    super::server::authenticate(&client, &headers)?;
    Ok(Json(load(&client, &id)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExperienceUpdate {
    title: String,
    body: String,
    /// 编辑基于的版本；与当前不一致时拒绝覆盖。
    #[serde(default)]
    expected_updated_at: Option<DateTime<Utc>>,
}

pub(super) async fn update(
    State(client): State<Client>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ExperienceUpdate>,
) -> Result<Json<ExperienceEntry>, ClientError> {
    super::server::authenticate(&client, &headers)?;
    let mut entry = load(&client, &id)?;
    if request
        .expected_updated_at
        .is_some_and(|expected| expected != entry.updated_at)
    {
        return Err(ClientError::Conflict("experience was modified elsewhere"));
    }
    entry
        .revise(&request.title, &request.body, Utc::now())
        .map_err(invalid)?;
    library(&client)?.save(&entry).map_err(storage)?;
    Ok(Json(entry))
}

pub(super) async fn delete(
    State(client): State<Client>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ClientError> {
    super::server::authenticate(&client, &headers)?;
    if !is_valid_experience_id(&id) {
        return Err(ClientError::NotFound);
    }
    if library(&client)?.delete(&id).map_err(storage)? {
        tracing::info!(event = "ExperienceDeleted", experience_id = %id, "经验已删除");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ClientError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_keeps_error_tail() {
        let text = format!("{}ERROR: permission denied", "x".repeat(5000));
        let kept = head_tail(&text, 10, 30);
        assert!(kept.ends_with("ERROR: permission denied"));
        assert!(kept.contains("中间省略"));
    }

    #[test]
    fn fit_budget_keeps_goal_and_recent_lines() {
        let mut lines = vec!["[用户] 目标".to_string()];
        lines.extend((0..100).map(|i| format!("[结果 shell] 第 {i} 行 {}", "x".repeat(50))));
        let text = fit_budget(&lines, 1_000);
        assert!(text.starts_with("[用户] 目标"));
        assert!(text.contains("第 99 行"));
        assert!(text.contains("中间省略"));
        assert!(text.chars().count() <= 1_100);
    }
}
