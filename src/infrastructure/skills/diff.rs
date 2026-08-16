use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::warn;

use crate::domain::SkillUpdateOperation;

/// 允许 LLM 修改的 frontmatter 字段白名单
pub const FRONTMATTER_WHITELIST: &[&str] =
    &["name", "description", "self_updatable", "dependencies"];

/// v8 D19：SKILL.md body 结构校验错误
#[derive(Debug, Error)]
pub enum SkillStructureError {
    #[error("instructions must contain at least one `##` heading")]
    NoSectionHeading,
    #[error("first `##` heading must have non-empty content")]
    EmptyFirstSection,
}

/// v8 D19：校验 SKILL.md body 结构
///
/// 规则：
/// 1. 至少包含 1 个 `## ` 二级标题
/// 2. 首个 `## ` 标题下必须有非空内容（到下一个 `##` 或 body 末尾之间至少 1 行非空）
pub fn validate_skill_structure(instructions: &str) -> Result<(), SkillStructureError> {
    let lines: Vec<&str> = instructions.lines().collect();
    let first_section_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with("## "))
        .ok_or(SkillStructureError::NoSectionHeading)?;
    // 首个 section 内容范围 = [first_section_idx + 1, 下一个 ## 或末尾)
    let end = lines
        .iter()
        .enumerate()
        .skip(first_section_idx + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    let has_non_empty = lines
        .iter()
        .skip(first_section_idx + 1)
        .take(end.saturating_sub(first_section_idx + 1))
        .any(|l| !l.trim().is_empty());
    if !has_non_empty {
        return Err(SkillStructureError::EmptyFirstSection);
    }
    Ok(())
}

/// 解析 SKILL.md，返回 frontmatter 部分和 body 部分
fn split_frontmatter(content: &str) -> (String, String) {
    let mut lines = content.lines();
    let first = lines.next();
    if first.map(|s| s.trim() != "---").unwrap_or(true) {
        return (String::new(), content.to_string());
    }
    let mut frontmatter = String::new();
    let mut body = String::new();
    let mut in_frontmatter = true;
    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            frontmatter.push_str(line);
            frontmatter.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    (frontmatter, body)
}

/// 找到 `## {section}` 章节的起始行号和结束行号（不含下一个 ## 标题）
///
/// v8 D19 修复 ADR-004 v7 已知局限 1 + 实现偏差 D：
/// - 标题行比较改用 `l.trim() == header.trim()`，避免尾部空格导致匹配失败
/// - 同层级同名章节匹配第一个时记录 warn 日志（v7 已知局限 1 落地）
fn find_section_range(body: &str, section: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let header = section.trim();
    let start = lines.iter().position(|l| l.trim() == header)?;
    // 检查是否存在同层级同名章节（已知局限 1：匹配第一个并记录 warn 日志）
    if lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .any(|(_, l)| l.trim() == header)
    {
        warn!(
            section = header,
            body_lines = lines.len(),
            "duplicate section heading, using first match"
        );
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Some((start, end))
}

/// 在 `## {section}` 范围内找到 `### {subsection}` 的起始行号和结束行号
///
/// v8 D19 新增。subsection 结束行 = 下一个 `###` 或 `##` 或父 section_end。
fn find_subsection_range(body: &str, section: &str, subsection: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let (section_start, section_end) = find_section_range(body, section)?;
    let subsection_header = subsection.trim();
    let start = lines
        .iter()
        .enumerate()
        .skip(section_start + 1)
        .take(section_end.saturating_sub(section_start + 1))
        .find(|(_, l)| l.trim() == subsection_header)
        .map(|(i, _)| i)?;
    // 同层级同名子章节匹配第一个并记录 warn 日志（与 find_section_range 一致）
    if lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .take(section_end.saturating_sub(start + 1))
        .any(|(_, l)| l.trim() == subsection_header)
    {
        warn!(
            section = section.trim(),
            subsection = subsection_header,
            body_lines = lines.len(),
            "duplicate subsection heading, using first match"
        );
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("### ") || l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(section_end);
    Some((start, end))
}

/// apply operations 到 SKILL.md 内容，返回更新后的内容
pub fn apply_skill_operations(
    content: &str,
    operations: &[SkillUpdateOperation],
) -> Result<String, ApplyError> {
    let (frontmatter, body) = split_frontmatter(content);
    let mut frontmatter_lines: Vec<String> = frontmatter.lines().map(|s| s.to_string()).collect();
    let mut body_lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();

    for op in operations {
        match op {
            SkillUpdateOperation::ReplaceSection {
                section,
                content,
                path: _,
            } => {
                let range = find_section_range(&body_lines.join("\n"), section)
                    .ok_or_else(|| ApplyError::SectionNotFound(section.clone()))?;
                // 保留 `## {section}` 行，替换后续内容
                body_lines.splice(range.0 + 1..range.1, content.lines().map(|s| s.to_string()));
            }
            SkillUpdateOperation::AddSection {
                after,
                section,
                content,
                path: _,
            } => {
                let body_str = body_lines.join("\n");
                let range = find_section_range(&body_str, after)
                    .ok_or_else(|| ApplyError::SectionNotFound(after.clone()))?;
                let mut new_lines: Vec<String> = vec![section.clone()];
                new_lines.extend(content.lines().map(|s| s.to_string()));
                new_lines.push(String::new()); // 空行分隔
                body_lines.splice(range.1..range.1, new_lines);
            }
            SkillUpdateOperation::RemoveSection { section, path: _ } => {
                let body_str = body_lines.join("\n");
                let range = find_section_range(&body_str, section)
                    .ok_or_else(|| ApplyError::SectionNotFound(section.clone()))?;
                body_lines.drain(range.0..range.1);
            }
            SkillUpdateOperation::ReplaceFrontmatter { field, value } => {
                if !FRONTMATTER_WHITELIST.contains(&field.as_str()) {
                    return Err(ApplyError::FieldNotWhitelisted(field.clone()));
                }
                let prefix = format!("{}:", field);
                if let Some(line) = frontmatter_lines
                    .iter_mut()
                    .find(|l| l.starts_with(&prefix))
                {
                    *line = format!("{}: {}", field, value);
                } else {
                    frontmatter_lines.push(format!("{}: {}", field, value));
                }
            }
            SkillUpdateOperation::ReplaceSubsection {
                section,
                subsection,
                content,
                path: _,
            } => {
                let body_str = body_lines.join("\n");
                let range =
                    find_subsection_range(&body_str, section, subsection).ok_or_else(|| {
                        ApplyError::SubsectionNotFound(section.clone(), subsection.clone())
                    })?;
                // 保留 `### {subsection}` 标题行，替换后续内容
                body_lines.splice(range.0 + 1..range.1, content.lines().map(|s| s.to_string()));
            }
            SkillUpdateOperation::AddSubsection {
                section,
                after,
                subsection,
                content,
                path: _,
            } => {
                let body_str = body_lines.join("\n");
                let range = find_subsection_range(&body_str, section, after).ok_or_else(|| {
                    ApplyError::SubsectionNotFound(section.clone(), after.clone())
                })?;
                let mut new_lines: Vec<String> = vec![subsection.clone()];
                new_lines.extend(content.lines().map(|s| s.to_string()));
                new_lines.push(String::new()); // 空行分隔
                body_lines.splice(range.1..range.1, new_lines);
            }
            SkillUpdateOperation::RemoveSubsection {
                section,
                subsection,
                path: _,
            } => {
                let body_str = body_lines.join("\n");
                let range =
                    find_subsection_range(&body_str, section, subsection).ok_or_else(|| {
                        ApplyError::SubsectionNotFound(section.clone(), subsection.clone())
                    })?;
                body_lines.drain(range.0..range.1);
            }
            SkillUpdateOperation::ReplaceBody { content, path: _ } => {
                body_lines = content.lines().map(|s| s.to_string()).collect();
            }
            // ADR-006 文件级操作在 apply_skill_operations_multi 中处理
            SkillUpdateOperation::ReplaceFile { .. }
            | SkillUpdateOperation::CreateFile { .. }
            | SkillUpdateOperation::DeleteFile { .. } => {
                return Err(ApplyError::MultiFileOperationInSingleFileContext);
            }
        }
    }

    // v8 D19：post-apply 结构校验，防止 LLM 用 replace_body 或 remove_section
    // 删除所有章节标题，整体回滚（D13 语义）
    let new_body: String = body_lines.join("\n");
    if let Err(e) = validate_skill_structure(&new_body) {
        return Err(ApplyError::StructureInvalid(e.to_string()));
    }

    let mut result = String::new();
    result.push_str("---\n");
    for line in &frontmatter_lines {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str("---\n\n");
    for line in &body_lines {
        result.push_str(line);
        result.push('\n');
    }
    Ok(result)
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("section not found: {0}")]
    SectionNotFound(String),
    #[error("subsection not found: {0} / {1}")]
    SubsectionNotFound(String, String),
    #[error("frontmatter field not whitelisted: {0}")]
    FieldNotWhitelisted(String),
    /// v8 D19：post-apply 结构校验失败（apply 后 body 无 `##` 标题或首个 section 空）
    #[error("post-apply structure invalid: {0}")]
    StructureInvalid(String),
    /// ADR-006：文件级操作在单文件 apply_skill_operations 中不可用
    #[error("multi-file operation not supported in single-file context")]
    MultiFileOperationInSingleFileContext,
    /// ADR-006：路径不在 skill 目录内或穿越了目录边界
    #[error("path escapes skill directory: {0}")]
    PathEscapesSkillDir(String),
    /// ADR-006：文件后缀不在白名单内
    #[error("file suffix not allowed: {0}")]
    SuffixNotAllowed(String),
    /// ADR-006：path 指向的 .md 文件不存在
    #[error("sibling markdown file not found: {0}")]
    SiblingFileNotFound(String),
    /// ADR-006：replace_file / delete_file 不可作用于 SKILL.md
    #[error("file operation not allowed on SKILL.md: {0}")]
    SkillMdNotAllowed(String),
    /// ADR-006：replace_file 要求文件已存在
    #[error("file not found for replace_file: {0}")]
    ReplaceFileNotFound(String),
    /// ADR-006：create_file 要求文件不存在
    #[error("file already exists for create_file: {0}")]
    CreateFileAlreadyExists(String),
    /// ADR-006：delete_file 要求文件已存在
    #[error("file not found for delete_file: {0}")]
    DeleteFileNotFound(String),
    /// ADR-006：section 级操作的 path 必须是 .md 后缀
    #[error("section operation path must be .md suffix: {0}")]
    SectionPathNotMd(String),
}

/// ADR-006：文件操作允许的后缀白名单
pub const ALLOWED_FILE_SUFFIXES: &[&str] = &["md", "py", "sh", "toml", "txt", "json"];

/// ADR-006：校验路径是否在 skill 目录内、后缀是否在白名单内。
///
/// - `rel_path`：相对于 skill 目录的路径
/// - `skill_dir`：skill 目录的绝对路径
/// - `allowed_suffixes`：允许的文件后缀列表（不含点号，如 `["md", "py"]`）
///
/// 返回解析后的绝对路径。
pub fn validate_skill_file_path(
    rel_path: &str,
    skill_dir: &Path,
    allowed_suffixes: &[&str],
) -> Result<PathBuf, ApplyError> {
    // 词法检查：拒绝绝对路径与 `..` 逃逸。
    // 依赖 canonicalize 的状态检查在中间目录不存在时会静默放行（canonicalize 失败），
    // 因此这里先用词法检查兜底，确保 create_file 不会借由 `..` 写穿 skill 目录。
    if std::path::Path::new(rel_path).is_absolute() {
        return Err(ApplyError::PathEscapesSkillDir(rel_path.to_string()));
    }
    let mut depth: i32 = 0;
    for comp in rel_path.split(['/', '\\']) {
        match comp {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(ApplyError::PathEscapesSkillDir(rel_path.to_string()));
                }
            }
            _ => depth += 1,
        }
    }

    // 拒绝路径穿越（状态检查：捕获 symlink 逃逸）
    let abs_path = skill_dir.join(rel_path);
    let canonical_skill_dir = skill_dir
        .canonicalize()
        .unwrap_or_else(|_| skill_dir.to_path_buf());
    // 对于不存在的文件，先检查父目录
    let check_path = if abs_path.exists() {
        abs_path.clone()
    } else {
        abs_path.parent().unwrap_or(skill_dir).to_path_buf()
    };
    if let Ok(canonical) = check_path.canonicalize()
        && !canonical.starts_with(&canonical_skill_dir)
    {
        return Err(ApplyError::PathEscapesSkillDir(rel_path.to_string()));
    }

    // 检查后缀
    let suffix = abs_path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if !allowed_suffixes.contains(&suffix) {
        return Err(ApplyError::SuffixNotAllowed(rel_path.to_string()));
    }

    Ok(abs_path)
}

/// ADR-006：对 sibling .md 文件执行 section 级操作。
///
/// 校验 path → 读取文件 → 构造 path:None 版本操作 → apply → 写回。
fn apply_sibling_md_section_op(
    skill_dir: &Path,
    p: &str,
    op: SkillUpdateOperation,
) -> Result<(), ApplyError> {
    // 校验 path：必须是 .md 后缀 + 文件已存在
    if !p.ends_with(".md") {
        return Err(ApplyError::SectionPathNotMd(p.to_string()));
    }
    let abs_path = validate_skill_file_path(p, skill_dir, &["md"])?;
    if !abs_path.exists() {
        return Err(ApplyError::SiblingFileNotFound(p.to_string()));
    }

    // 读取文件内容
    let file_content = std::fs::read_to_string(&abs_path)
        .map_err(|_| ApplyError::SiblingFileNotFound(p.to_string()))?;

    // 构造 path:None 版本的操作用于 apply
    let single_op = strip_path_from_op(&op);
    let new_content = apply_skill_operations(&file_content, &[single_op])?;
    std::fs::write(&abs_path, &new_content)
        .map_err(|e| ApplyError::SiblingFileNotFound(format!("write failed: {}", e)))?;
    Ok(())
}

/// 从 SkillUpdateOperation 中移除 path 字段，构造 path:None 版本。
fn strip_path_from_op(op: &SkillUpdateOperation) -> SkillUpdateOperation {
    match op {
        SkillUpdateOperation::ReplaceSection {
            section, content, ..
        } => SkillUpdateOperation::ReplaceSection {
            section: section.clone(),
            content: content.clone(),
            path: None,
        },
        SkillUpdateOperation::AddSection {
            after,
            section,
            content,
            ..
        } => SkillUpdateOperation::AddSection {
            after: after.clone(),
            section: section.clone(),
            content: content.clone(),
            path: None,
        },
        SkillUpdateOperation::RemoveSection { section, .. } => {
            SkillUpdateOperation::RemoveSection {
                section: section.clone(),
                path: None,
            }
        }
        SkillUpdateOperation::ReplaceSubsection {
            section,
            subsection,
            content,
            ..
        } => SkillUpdateOperation::ReplaceSubsection {
            section: section.clone(),
            subsection: subsection.clone(),
            content: content.clone(),
            path: None,
        },
        SkillUpdateOperation::AddSubsection {
            section,
            after,
            subsection,
            content,
            ..
        } => SkillUpdateOperation::AddSubsection {
            section: section.clone(),
            after: after.clone(),
            subsection: subsection.clone(),
            content: content.clone(),
            path: None,
        },
        SkillUpdateOperation::RemoveSubsection {
            section,
            subsection,
            ..
        } => SkillUpdateOperation::RemoveSubsection {
            section: section.clone(),
            subsection: subsection.clone(),
            path: None,
        },
        SkillUpdateOperation::ReplaceBody { content, .. } => SkillUpdateOperation::ReplaceBody {
            content: content.clone(),
            path: None,
        },
        // 以下操作不需要 strip path
        SkillUpdateOperation::ReplaceFrontmatter { field, value } => {
            SkillUpdateOperation::ReplaceFrontmatter {
                field: field.clone(),
                value: value.clone(),
            }
        }
        SkillUpdateOperation::ReplaceFile { path, content } => SkillUpdateOperation::ReplaceFile {
            path: path.clone(),
            content: content.clone(),
        },
        SkillUpdateOperation::CreateFile { path, content } => SkillUpdateOperation::CreateFile {
            path: path.clone(),
            content: content.clone(),
        },
        SkillUpdateOperation::DeleteFile { path } => {
            SkillUpdateOperation::DeleteFile { path: path.clone() }
        }
    }
}

/// ADR-006：多文件 skill 的 apply 操作。
///
/// 遍历 operations，根据 `path` 字段决定操作目标：
/// - `path: None` → SKILL.md（复用 `apply_skill_operations`）
/// - `path: Some(p)` → 校验后对 sibling `.md` 文件执行 section 级操作
/// - `ReplaceFile` / `CreateFile` / `DeleteFile` → 文件级操作
///
/// **注意**：调用方负责在调用前做目录级快照备份，失败时回滚。
/// 本函数只负责 apply，不负责备份/回滚。
pub fn apply_skill_operations_multi(
    skill_dir: &Path,
    operations: &[SkillUpdateOperation],
) -> Result<(), ApplyError> {
    let skill_md_path = skill_dir.join("SKILL.md");

    // 分离 SKILL.md 操作和文件级操作
    let mut skill_md_ops: Vec<SkillUpdateOperation> = Vec::new();
    for op in operations {
        match op {
            // path: None → SKILL.md 操作，收集后批量 apply
            SkillUpdateOperation::ReplaceSection { path: None, .. }
            | SkillUpdateOperation::AddSection { path: None, .. }
            | SkillUpdateOperation::RemoveSection { path: None, .. }
            | SkillUpdateOperation::ReplaceSubsection { path: None, .. }
            | SkillUpdateOperation::AddSubsection { path: None, .. }
            | SkillUpdateOperation::RemoveSubsection { path: None, .. }
            | SkillUpdateOperation::ReplaceBody { path: None, .. }
            | SkillUpdateOperation::ReplaceFrontmatter { .. } => {
                skill_md_ops.push(op.clone());
            }

            // path: Some(p) → sibling .md 文件的 section 级操作
            SkillUpdateOperation::ReplaceSection {
                section: _,
                content: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }
            SkillUpdateOperation::AddSection {
                after: _,
                section: _,
                content: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }
            SkillUpdateOperation::RemoveSection {
                section: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }
            SkillUpdateOperation::ReplaceSubsection {
                section: _,
                subsection: _,
                content: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }
            SkillUpdateOperation::AddSubsection {
                section: _,
                after: _,
                subsection: _,
                content: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }
            SkillUpdateOperation::RemoveSubsection {
                section: _,
                subsection: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }
            SkillUpdateOperation::ReplaceBody {
                content: _,
                path: Some(p),
            } => {
                apply_sibling_md_section_op(skill_dir, p, op.clone())?;
            }

            // ADR-006 文件级操作
            SkillUpdateOperation::ReplaceFile { path, content } => {
                if path == "SKILL.md" {
                    return Err(ApplyError::SkillMdNotAllowed(path.clone()));
                }
                let abs_path = validate_skill_file_path(path, skill_dir, ALLOWED_FILE_SUFFIXES)?;
                if !abs_path.exists() {
                    return Err(ApplyError::ReplaceFileNotFound(path.clone()));
                }
                std::fs::write(&abs_path, content)
                    .map_err(|e| ApplyError::ReplaceFileNotFound(format!("write failed: {}", e)))?;
            }
            SkillUpdateOperation::CreateFile { path, content } => {
                if path == "SKILL.md" {
                    return Err(ApplyError::SkillMdNotAllowed(path.clone()));
                }
                let abs_path = validate_skill_file_path(path, skill_dir, ALLOWED_FILE_SUFFIXES)?;
                if abs_path.exists() {
                    return Err(ApplyError::CreateFileAlreadyExists(path.clone()));
                }
                // 确保父目录存在
                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        ApplyError::CreateFileAlreadyExists(format!("mkdir failed: {}", e))
                    })?;
                }
                std::fs::write(&abs_path, content).map_err(|e| {
                    ApplyError::CreateFileAlreadyExists(format!("write failed: {}", e))
                })?;
            }
            SkillUpdateOperation::DeleteFile { path } => {
                if path == "SKILL.md" {
                    return Err(ApplyError::SkillMdNotAllowed(path.clone()));
                }
                let abs_path = validate_skill_file_path(path, skill_dir, ALLOWED_FILE_SUFFIXES)?;
                if !abs_path.exists() {
                    return Err(ApplyError::DeleteFileNotFound(path.clone()));
                }
                std::fs::remove_file(&abs_path)
                    .map_err(|e| ApplyError::DeleteFileNotFound(format!("remove failed: {}", e)))?;
            }
        }
    }

    // Apply SKILL.md 操作（如果有）
    if !skill_md_ops.is_empty() {
        let content = std::fs::read_to_string(&skill_md_path)
            .map_err(|e| ApplyError::SectionNotFound(format!("read SKILL.md failed: {}", e)))?;
        let new_content = apply_skill_operations(&content, &skill_md_ops)?;
        std::fs::write(&skill_md_path, &new_content)
            .map_err(|e| ApplyError::SectionNotFound(format!("write SKILL.md failed: {}", e)))?;
    }

    Ok(())
}

/// ADR-006：目录级快照备份。
///
/// 将整个 skill 目录复制到 `history/v{version}/`。
/// 排除 `history/` 目录本身，避免递归复制。
pub fn backup_skill_dir(skill_dir: &Path, version: u32) -> std::io::Result<PathBuf> {
    let history_dir = skill_dir.join("history");
    let backup_dir = history_dir.join(format!("v{}", version));
    std::fs::create_dir_all(&backup_dir)?;

    copy_dir_recursive(skill_dir, &backup_dir, &history_dir)?;
    Ok(backup_dir)
}

/// ADR-006：从目录级快照恢复 skill 目录。
///
/// 清空 skill 目录（排除 `history/`），然后将备份目录内容复制回来。
pub fn restore_skill_dir(skill_dir: &Path, backup_dir: &Path) -> std::io::Result<()> {
    let history_dir = skill_dir.join("history");

    // 清空当前 skill 目录（保留 history/）
    for entry in std::fs::read_dir(skill_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path == history_dir {
            continue; // 保留 history 目录
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }

    // 从备份恢复
    copy_dir_recursive(backup_dir, skill_dir, &history_dir)?;
    Ok(())
}

/// ADR-006：清理目录级 history，保留最新 `keep` 代。
pub fn cleanup_skill_dir_history(history_dir: &Path, keep: usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(history_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut versions: Vec<(u32, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(stripped) = file_name.strip_prefix('v')
            && let Ok(v) = stripped.parse::<u32>()
        {
            versions.push((v, path));
        }
    }

    versions.sort_by_key(|(v, _)| *v);
    let excess = versions.len().saturating_sub(keep);
    for (_, path) in versions.iter().take(excess) {
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

/// 递归复制目录，排除 `exclude` 路径。
fn copy_dir_recursive(src: &Path, dst: &Path, exclude: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        if src_path == exclude {
            continue;
        }
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path, &exclude.join(entry.file_name()))?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// 在 SKILL.md 内容的 frontmatter 中设置 version 字段（upsert 语义）。
///
/// 由系统在 skill update 写入前调用，确保版本号持久化到文件，
/// 避免重启后 `parse_skill_md` 因缺少 version 字段而默认回退到 1。
pub fn set_frontmatter_version(content: &str, version: u32) -> String {
    let (frontmatter, body) = split_frontmatter(content);
    let mut lines: Vec<String> = frontmatter.lines().map(|s| s.to_string()).collect();
    let prefix = "version:";
    if let Some(line) = lines.iter_mut().find(|l| l.starts_with(prefix)) {
        *line = format!("version: {}", version);
    } else {
        lines.push(format!("version: {}", version));
    }
    let mut result = String::new();
    result.push_str("---\n");
    for line in &lines {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str("---\n\n");
    result.push_str(&body);
    result
}

/// 保留最新 keep 代历史，删除超出部分
pub fn cleanup_skill_history(history_dir: &Path, keep: usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(history_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut versions: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // 解析 vN.md
        if let Some(stripped) = file_name.strip_prefix('v')
            && let Some(name) = stripped.strip_suffix(".md")
            && let Ok(v) = name.parse::<u32>()
        {
            versions.push((v, path));
        }
    }

    versions.sort_by_key(|(v, _)| *v);
    let excess = versions.len().saturating_sub(keep);
    for (_, path) in versions.iter().take(excess) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: test\ndescription: A skill\nversion: 1\n---\n\n## Usage\n\nDo it.\n\n## Examples\n\nExample 1.\n";
    const SAMPLE_WITH_SUBSECTIONS: &str = "---\nname: test\ndescription: A skill\n---\n\n## Usage\n\n### Basic\n\nDo step 1.\n\n### Advanced\n\nDo step 2.\n\n## Examples\n\nExample 1.\n";
    /// 跨 section 同名 subsection 样本：`## Usage` 与 `## Examples` 都含 `### Common`。
    /// 用于回归测试 v8 D19 `find_subsection_range` 的父 section 范围硬限制：
    /// 不同父 section 下的同名 subsection 必须互不干扰。
    const SAMPLE_CROSS_SECTION_SAME_SUBSECTION: &str = "---\nname: test\ndescription: A skill\n---\n\n## Usage\n\n### Common\n\nUsage common content.\n\n### Specific\n\nUsage specific content.\n\n## Examples\n\n### Common\n\nExamples common content.\n";

    #[test]
    fn replace_section_existing() {
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Usage".to_string(),
            content: "New usage content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("New usage content."));
        assert!(!result.contains("Do it."));
    }

    #[test]
    fn replace_section_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Missing".to_string(),
            content: "x".to_string(),
            path: None,
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE, &ops),
            Err(ApplyError::SectionNotFound(_))
        ));
    }

    #[test]
    fn add_section_after_existing() {
        let ops = vec![SkillUpdateOperation::AddSection {
            after: "## Usage".to_string(),
            section: "## Edge Cases".to_string(),
            content: "Edge case notes.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("## Edge Cases"));
        assert!(result.contains("Edge case notes."));
        let usage_idx = result.find("## Usage").unwrap();
        let edge_idx = result.find("## Edge Cases").unwrap();
        let examples_idx = result.find("## Examples").unwrap();
        assert!(usage_idx < edge_idx);
        assert!(edge_idx < examples_idx);
    }

    #[test]
    fn remove_section_existing() {
        let ops = vec![SkillUpdateOperation::RemoveSection {
            section: "## Examples".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(!result.contains("## Examples"));
        assert!(!result.contains("Example 1."));
    }

    #[test]
    fn replace_frontmatter_in_whitelist() {
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "description".to_string(),
            value: "Updated description".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("description: Updated description"));
    }

    #[test]
    fn replace_frontmatter_not_in_whitelist() {
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "version".to_string(),
            value: "999".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE, &ops),
            Err(ApplyError::FieldNotWhitelisted(_))
        ));
    }

    #[test]
    fn replace_frontmatter_dependencies_in_whitelist() {
        // 单行 YAML 数组值直接拼接为 `{field}: {value}`，不返回 FieldNotWhitelisted
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "dependencies".to_string(),
            value: "[browser-automation]".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("dependencies: [browser-automation]"));
    }

    #[test]
    fn replace_frontmatter_upsert_appends_missing_field() {
        // v8 D19：upsert 语义 — 字段不存在则追加
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "self_updatable".to_string(),
            value: "false".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("self_updatable: false"));
    }

    #[test]
    fn replace_subsection_existing() {
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Usage".to_string(),
            subsection: "### Advanced".to_string(),
            content: "Do step 2 with caution.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops).unwrap();
        assert!(result.contains("Do step 2 with caution."));
        assert!(!result.contains("Do step 2."));
        // 其他子章节不变
        assert!(result.contains("Do step 1."));
    }

    #[test]
    fn replace_subsection_section_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Missing".to_string(),
            subsection: "### Advanced".to_string(),
            content: "x".to_string(),
            path: None,
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops),
            Err(ApplyError::SubsectionNotFound(_, _))
        ));
    }

    #[test]
    fn replace_subsection_subsection_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Usage".to_string(),
            subsection: "### Missing".to_string(),
            content: "x".to_string(),
            path: None,
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops),
            Err(ApplyError::SubsectionNotFound(_, _))
        ));
    }

    #[test]
    fn add_subsection_after_existing() {
        let ops = vec![SkillUpdateOperation::AddSubsection {
            section: "## Usage".to_string(),
            after: "### Basic".to_string(),
            subsection: "### Edge Cases".to_string(),
            content: "Edge case content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops).unwrap();
        assert!(result.contains("### Edge Cases"));
        assert!(result.contains("Edge case content."));
        let basic_idx = result.find("### Basic").unwrap();
        let edge_idx = result.find("### Edge Cases").unwrap();
        let advanced_idx = result.find("### Advanced").unwrap();
        assert!(basic_idx < edge_idx);
        assert!(edge_idx < advanced_idx);
    }

    #[test]
    fn add_subsection_after_not_found() {
        let ops = vec![SkillUpdateOperation::AddSubsection {
            section: "## Usage".to_string(),
            after: "### Missing".to_string(),
            subsection: "### New".to_string(),
            content: "x".to_string(),
            path: None,
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops),
            Err(ApplyError::SubsectionNotFound(_, _))
        ));
    }

    #[test]
    fn remove_subsection_existing() {
        let ops = vec![SkillUpdateOperation::RemoveSubsection {
            section: "## Usage".to_string(),
            subsection: "### Advanced".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops).unwrap();
        assert!(!result.contains("### Advanced"));
        assert!(!result.contains("Do step 2."));
        // 其他子章节不变
        assert!(result.contains("### Basic"));
        assert!(result.contains("Do step 1."));
    }

    // ============ v8 D19 回归：跨 section 同名 subsection 必须互不干扰 ============
    //
    // `find_subsection_range` 通过父 section 范围硬限制隔离同名 subsection。
    // 这些测试确保未来重构不会破坏该不变量。

    #[test]
    fn replace_subsection_only_affects_target_section_when_names_collide() {
        // 替换 `## Usage` 下的 `### Common`，不应影响 `## Examples` 下的 `### Common`
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Usage".to_string(),
            subsection: "### Common".to_string(),
            content: "New usage common content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_CROSS_SECTION_SAME_SUBSECTION, &ops).unwrap();
        // 目标 subsection 已更新
        assert!(
            result.contains("New usage common content."),
            "target subsection should be updated; got:\n{}",
            result
        );
        assert!(
            !result.contains("Usage common content."),
            "old target subsection content should be gone; got:\n{}",
            result
        );
        // 非目标 section 下的同名 subsection 保持不变
        assert!(
            result.contains("Examples common content."),
            "non-target same-name subsection must remain untouched; got:\n{}",
            result
        );
        // `### Specific` 不受影响
        assert!(
            result.contains("Usage specific content."),
            "sibling subsection should be untouched; got:\n{}",
            result
        );
    }

    #[test]
    fn add_subsection_does_not_cross_section_boundary_when_names_collide() {
        // 在 `## Usage` 的 `### Common` 之后插入 `### New`，
        // 不应插入到 `## Examples` 范围内或其后
        let ops = vec![SkillUpdateOperation::AddSubsection {
            section: "## Usage".to_string(),
            after: "### Common".to_string(),
            subsection: "### New".to_string(),
            content: "New subsection content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_CROSS_SECTION_SAME_SUBSECTION, &ops).unwrap();
        // 新 subsection 已添加
        assert!(
            result.contains("### New"),
            "new subsection should be added; got:\n{}",
            result
        );
        assert!(
            result.contains("New subsection content."),
            "new subsection content should be present; got:\n{}",
            result
        );
        // 关键不变量：`### New` 必须位于 `## Usage` 范围内（在 `### Specific` 之前，
        // `## Examples` 之前），不应跨越到 `## Examples` 范围
        let usage_idx = result.find("## Usage").unwrap();
        let new_idx = result.find("### New").unwrap();
        let specific_idx = result.find("### Specific").unwrap();
        let examples_idx = result.find("## Examples").unwrap();
        assert!(
            usage_idx < new_idx && new_idx < specific_idx && new_idx < examples_idx,
            "### New must be inside ## Usage range; got positions: usage={}, new={}, specific={}, examples={}",
            usage_idx,
            new_idx,
            specific_idx,
            examples_idx
        );
    }

    #[test]
    fn remove_subsection_only_removes_from_target_section_when_names_collide() {
        // 删除 `## Usage` 下的 `### Common`，不应删除 `## Examples` 下的 `### Common`
        let ops = vec![SkillUpdateOperation::RemoveSubsection {
            section: "## Usage".to_string(),
            subsection: "### Common".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_CROSS_SECTION_SAME_SUBSECTION, &ops).unwrap();
        // 目标 subsection 已删除（连同其内容）
        assert!(
            !result.contains("Usage common content."),
            "target subsection content should be removed; got:\n{}",
            result
        );
        // 非目标 section 下的同名 subsection 必须保留
        assert!(
            result.contains("Examples common content."),
            "non-target same-name subsection must remain; got:\n{}",
            result
        );
        // `## Examples` 下的 `### Common` 标题仍在
        // 通过统计 `### Common` 出现次数验证：原 2 次，删除后应剩 1 次
        let common_count = result.matches("### Common").count();
        assert_eq!(
            common_count, 1,
            "only one ### Common (in ## Examples) should remain; got:\n{}",
            result
        );
        // `### Specific` 不受影响
        assert!(
            result.contains("Usage specific content."),
            "sibling subsection should be untouched; got:\n{}",
            result
        );
    }

    #[test]
    fn replace_subsection_in_second_section_does_not_touch_first_when_names_collide() {
        // 对称场景：替换 `## Examples` 下的 `### Common`，不应影响 `## Usage` 下的 `### Common`
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Examples".to_string(),
            subsection: "### Common".to_string(),
            content: "New examples common content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE_CROSS_SECTION_SAME_SUBSECTION, &ops).unwrap();
        // 目标 subsection 已更新
        assert!(
            result.contains("New examples common content."),
            "target subsection should be updated; got:\n{}",
            result
        );
        assert!(
            !result.contains("Examples common content."),
            "old target subsection content should be gone; got:\n{}",
            result
        );
        // 非目标 section 下的同名 subsection 保持不变
        assert!(
            result.contains("Usage common content."),
            "non-target same-name subsection must remain untouched; got:\n{}",
            result
        );
    }

    #[test]
    fn replace_body_replaces_body_keeps_frontmatter() {
        let ops = vec![SkillUpdateOperation::ReplaceBody {
            content: "## New Section\n\nNew body content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        // frontmatter 保留
        assert!(result.contains("name: test"));
        assert!(result.contains("description: A skill"));
        // body 被整体替换
        assert!(result.contains("## New Section"));
        assert!(result.contains("New body content."));
        // 原 body 内容消失
        assert!(!result.contains("Do it."));
        assert!(!result.contains("## Usage"));
    }

    #[test]
    fn find_section_range_trailing_whitespace_matches() {
        // v8 D19 修复 ADR-004 v7 实现偏差 D：trim_start → trim，尾部空格也能匹配
        let body = "## Usage \n\nDo it.\n";
        let range = find_section_range(body, "## Usage").unwrap();
        assert_eq!(range.0, 0);
    }

    #[test]
    fn validate_skill_structure_compliant() {
        let body = "## Usage\n\nDo it.\n\n## Examples\n\nExample.\n";
        assert!(validate_skill_structure(body).is_ok());
    }

    #[test]
    fn validate_skill_structure_no_section_heading() {
        let body = "Just plain text without any heading.";
        assert!(matches!(
            validate_skill_structure(body),
            Err(SkillStructureError::NoSectionHeading)
        ));
    }

    #[test]
    fn validate_skill_structure_empty_first_section() {
        // 首个 ## 标题下到下一个 ## 之间无非空内容
        let body = "## First\n\n## Second\n\nContent.\n";
        assert!(matches!(
            validate_skill_structure(body),
            Err(SkillStructureError::EmptyFirstSection)
        ));
    }

    #[test]
    fn apply_replace_body_with_compliant_structure_passes() {
        // v8 D19：replace_body 后 body 仍有 ## 标题，应通过 post-apply 校验
        let ops = vec![SkillUpdateOperation::ReplaceBody {
            content: "## New Section\n\nNew content.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE, &ops);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_replace_body_with_no_section_heading_rolls_back() {
        // v8 D19：replace_body 后 body 无 ## 标题，post-apply 校验应返回 StructureInvalid
        let ops = vec![SkillUpdateOperation::ReplaceBody {
            content: "Just plain text without heading.".to_string(),
            path: None,
        }];
        let result = apply_skill_operations(SAMPLE, &ops);
        assert!(matches!(result, Err(ApplyError::StructureInvalid(_))));
    }

    #[test]
    fn apply_remove_all_sections_rolls_back() {
        // v8 D19：删除所有 ## 章节后 body 无 ## 标题，应回滚
        let ops = vec![
            SkillUpdateOperation::RemoveSection {
                section: "## Usage".to_string(),
                path: None,
            },
            SkillUpdateOperation::RemoveSection {
                section: "## Examples".to_string(),
                path: None,
            },
        ];
        let result = apply_skill_operations(SAMPLE, &ops);
        assert!(matches!(result, Err(ApplyError::StructureInvalid(_))));
    }

    #[test]
    fn set_frontmatter_version_updates_existing_field() {
        // SAMPLE 已含 version: 1，应被替换为 version: 3
        let result = set_frontmatter_version(SAMPLE, 3);
        assert!(result.contains("version: 3"), "got:\n{}", result);
        assert!(
            !result.contains("version: 1"),
            "old version should be replaced"
        );
        // body 内容不受影响
        assert!(result.contains("## Usage"));
        assert!(result.contains("Do it."));
    }

    #[test]
    fn set_frontmatter_version_appends_when_missing() {
        // SAMPLE_WITH_SUBSECTIONS 无 version 字段，应追加
        let result = set_frontmatter_version(SAMPLE_WITH_SUBSECTIONS, 2);
        assert!(result.contains("version: 2"), "got:\n{}", result);
        // body 内容不受影响
        assert!(result.contains("## Usage"));
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cleanup_keeps_latest_3_generations() {
        let tmp = TempDir::new().unwrap();
        let history_dir = tmp.path().join("history");
        fs::create_dir_all(&history_dir).unwrap();
        for v in 1..=6 {
            fs::write(history_dir.join(format!("v{}.md", v)), format!("v{}", v)).unwrap();
        }
        cleanup_skill_history(&history_dir, 3).unwrap();
        let remaining: Vec<_> = fs::read_dir(&history_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), 3);
        // 保留最新的 3 代（v4, v5, v6）
        assert!(remaining.contains(&"v4.md".to_string()));
        assert!(remaining.contains(&"v5.md".to_string()));
        assert!(remaining.contains(&"v6.md".to_string()));
    }

    #[test]
    fn cleanup_no_dir_is_noop() {
        let result = cleanup_skill_history(std::path::Path::new("/nonexistent"), 3);
        assert!(result.is_ok());
    }
}

/// ADR-006：多文件 apply、路径校验、目录级快照备份/回滚测试。
#[cfg(test)]
mod multi_file_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const SKILL_MD: &str =
        "---\nname: test\ndescription: A skill\nversion: 1\n---\n\n## Usage\n\nDo it.\n";

    /// 构造一个含 SKILL.md + sibling download.md 的临时 skill 目录。
    fn setup_skill_dir() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("SKILL.md"), SKILL_MD).unwrap();
        fs::write(
            dir.join("download.md"),
            "# Download\n\n## Steps\n\nOld steps.\n",
        )
        .unwrap();
        fs::write(dir.join("scripts/run.py"), "print('old')").unwrap();
        (tmp, dir)
    }

    // ---- validate_skill_file_path ----

    #[test]
    fn validate_path_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(dir.join("SKILL.md"), SKILL_MD).unwrap();
        assert!(matches!(
            validate_skill_file_path("../outside.md", &dir, &["md"]),
            Err(ApplyError::PathEscapesSkillDir(_))
        ));
        assert!(matches!(
            validate_skill_file_path("../../etc/passwd", &dir, &["md"]),
            Err(ApplyError::PathEscapesSkillDir(_))
        ));
    }

    #[test]
    fn validate_path_rejects_disallowed_suffix() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        assert!(matches!(
            validate_skill_file_path("evil.rs", &dir, &["md"]),
            Err(ApplyError::SuffixNotAllowed(_))
        ));
    }

    #[test]
    fn validate_path_accepts_allowed_suffix_in_subdir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/run.py"), "x").unwrap();
        let abs = validate_skill_file_path("scripts/run.py", &dir, &["py"]).unwrap();
        assert!(abs.ends_with("scripts/run.py"));
    }

    // ---- apply_skill_operations_multi ----

    #[test]
    fn multi_apply_skill_md_section_and_sibling_section() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![
            SkillUpdateOperation::ReplaceSection {
                section: "## Usage".to_string(),
                content: "New usage.".to_string(),
                path: None,
            },
            SkillUpdateOperation::ReplaceSection {
                section: "## Steps".to_string(),
                content: "New steps.".to_string(),
                path: Some("download.md".to_string()),
            },
        ];
        apply_skill_operations_multi(&dir, &ops).unwrap();

        let skill_md = fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert!(skill_md.contains("New usage."));
        assert!(!skill_md.contains("Do it."));

        let sibling = fs::read_to_string(dir.join("download.md")).unwrap();
        assert!(sibling.contains("New steps."));
        assert!(!sibling.contains("Old steps."));
    }

    #[test]
    fn multi_apply_file_operations() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![
            SkillUpdateOperation::ReplaceFile {
                path: "scripts/run.py".to_string(),
                content: "print('new')".to_string(),
            },
            SkillUpdateOperation::CreateFile {
                path: "templates/note.md".to_string(),
                content: "# Note".to_string(),
            },
            SkillUpdateOperation::DeleteFile {
                path: "download.md".to_string(),
            },
        ];
        apply_skill_operations_multi(&dir, &ops).unwrap();

        let py = fs::read_to_string(dir.join("scripts/run.py")).unwrap();
        assert_eq!(py, "print('new')");
        assert!(dir.join("templates/note.md").exists());
        assert!(!dir.join("download.md").exists());
    }

    #[test]
    fn multi_apply_rejects_skill_md_file_operations() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![SkillUpdateOperation::ReplaceFile {
            path: "SKILL.md".to_string(),
            content: "nope".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::SkillMdNotAllowed(_))
        ));
        let ops = vec![SkillUpdateOperation::DeleteFile {
            path: "SKILL.md".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::SkillMdNotAllowed(_))
        ));
    }

    #[test]
    fn multi_apply_rejects_replace_nonexistent_file() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![SkillUpdateOperation::ReplaceFile {
            path: "missing.md".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::ReplaceFileNotFound(_))
        ));
    }

    #[test]
    fn multi_apply_rejects_create_existing_file() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![SkillUpdateOperation::CreateFile {
            path: "download.md".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::CreateFileAlreadyExists(_))
        ));
    }

    #[test]
    fn multi_apply_rejects_delete_nonexistent_file() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![SkillUpdateOperation::DeleteFile {
            path: "ghost.md".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::DeleteFileNotFound(_))
        ));
    }

    #[test]
    fn multi_apply_rejects_sibling_section_on_non_md_or_missing() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Steps".to_string(),
            content: "x".to_string(),
            path: Some("run.py".to_string()),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::SectionPathNotMd(_))
        ));

        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Steps".to_string(),
            content: "x".to_string(),
            path: Some("missing.md".to_string()),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::SiblingFileNotFound(_))
        ));
    }

    #[test]
    fn multi_apply_single_file_ops_path_escapes_rejected() {
        let (_tmp, dir) = setup_skill_dir();
        let ops = vec![SkillUpdateOperation::ReplaceFile {
            path: "../evil.py".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations_multi(&dir, &ops),
            Err(ApplyError::PathEscapesSkillDir(_))
        ));
    }

    #[test]
    fn single_file_apply_rejects_file_level_ops() {
        // 向后兼容：文件级操作在 apply_skill_operations 中必须报错
        let ops = vec![SkillUpdateOperation::ReplaceFile {
            path: "x.md".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SKILL_MD, &ops),
            Err(ApplyError::MultiFileOperationInSingleFileContext)
        ));
    }

    // ---- backup / restore / cleanup ----

    #[test]
    fn backup_captures_all_files_excluding_history() {
        let (_tmp, dir) = setup_skill_dir();
        let backup = backup_skill_dir(&dir, 1).unwrap();
        assert!(backup.join("SKILL.md").exists());
        assert!(backup.join("download.md").exists());
        assert!(backup.join("scripts/run.py").exists());
        // 备份内不含嵌套 history
        assert!(!backup.join("history").exists());
    }

    #[test]
    fn restore_reverts_directory_to_snapshot() {
        let (_tmp, dir) = setup_skill_dir();
        backup_skill_dir(&dir, 1).unwrap();
        // 修改文件 + 新增文件 + 删除文件
        fs::write(dir.join("SKILL.md"), "changed").unwrap();
        fs::write(dir.join("new.md"), "new").unwrap();
        fs::remove_file(dir.join("download.md")).unwrap();

        let backup = dir.join("history").join("v1");
        restore_skill_dir(&dir, &backup).unwrap();

        let skill_md = fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert!(skill_md.contains("Do it."));
        assert!(dir.join("download.md").exists());
        assert!(!dir.join("new.md").exists());
    }

    #[test]
    fn restore_preserves_history_dir() {
        let (_tmp, dir) = setup_skill_dir();
        backup_skill_dir(&dir, 1).unwrap();
        let backup = dir.join("history").join("v1");
        restore_skill_dir(&dir, &backup).unwrap();
        // 回滚后 history 目录仍保留
        assert!(dir.join("history").join("v1").exists());
    }

    #[test]
    fn cleanup_dir_history_keeps_latest_n() {
        let (_tmp, dir) = setup_skill_dir();
        for v in 1..=6 {
            backup_skill_dir(&dir, v).unwrap();
        }
        let history = dir.join("history");
        cleanup_skill_dir_history(&history, 3).unwrap();
        let remaining: Vec<String> = fs::read_dir(&history)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), 3);
        assert!(remaining.contains(&"v4".to_string()));
        assert!(remaining.contains(&"v5".to_string()));
        assert!(remaining.contains(&"v6".to_string()));
    }

    #[test]
    fn cleanup_dir_history_no_dir_is_noop() {
        let result = cleanup_skill_dir_history(std::path::Path::new("/nonexistent"), 3);
        assert!(result.is_ok());
    }
}
