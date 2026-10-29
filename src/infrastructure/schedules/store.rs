//! JSON 文件定时任务存储
//!
//! 单文件保存全部定义，写入走"临时文件 + rename"原子替换；文件损坏时改名备份后按空处理，
//! 避免一次坏写阻断进程启动。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use bevy_ecs::prelude::Resource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::domain::{RunOutcome, RunSummary, ScheduleDefinition, ScheduleRunRecord};
use uuid::Uuid;

/// 当前存储格式版本。
const STORE_VERSION: u32 = 1;

/// 定时任务存储错误。
#[derive(Debug, thiserror::Error)]
pub enum ScheduleStoreError {
    #[error("schedule store I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("schedule store serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("schedule run ledger JSON failed at {path}, line {line}: {source}")]
    LedgerRecord {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    schedules: Vec<ScheduleDefinition>,
}

/// 动态定时任务的文件存储。
///
/// 作为 Resource 插入 World 后，`schedule_persist_system` 在 `SchedulerState` 变化时写盘；
/// 未插入时（测试默认）不落盘。
#[derive(Resource, Debug, Clone)]
pub struct ScheduleStore {
    path: PathBuf,
}

impl ScheduleStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 默认路径 `.harness/schedules/schedules.json`。
    pub fn default_path() -> Self {
        Self::new(".harness/schedules/schedules.json")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state_dir(&self, id: Uuid) -> PathBuf {
        self.schedule_dir(id).join("state")
    }

    fn schedule_dir(&self, id: Uuid) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(id.to_string())
    }

    /// harness 单写者追加账本；先校验已有记录，损坏时保留原文件并返回定位错误。
    /// 任务按 task_id 去重，未执行触发按时间与结果去重，重试不会重复记账。
    pub fn append_run(&self, record: &ScheduleRunRecord) -> Result<(), ScheduleStoreError> {
        let records = self.read_runs(record.schedule_id)?;
        if records.iter().any(|old| Self::same_run(old, record)) {
            return Ok(());
        }
        let path = self.schedule_dir(record.schedule_id).join("runs.jsonl");
        fs::create_dir_all(path.parent().unwrap()).map_err(|source| ScheduleStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .map_err(|source| ScheduleStoreError::Io {
                path: path.clone(),
                source,
            })?;
        // 无换行的完整末条也有效；下条追加前补分隔符。
        let len = file
            .metadata()
            .map_err(|source| ScheduleStoreError::Io {
                path: path.clone(),
                source,
            })?
            .len();
        if len > 0 {
            use std::io::{Read, Seek, SeekFrom};
            file.seek(SeekFrom::End(-1))
                .map_err(|source| ScheduleStoreError::Io {
                    path: path.clone(),
                    source,
                })?;
            let mut last = [0];
            file.read_exact(&mut last)
                .map_err(|source| ScheduleStoreError::Io {
                    path: path.clone(),
                    source,
                })?;
            if last[0] != b'\n' {
                bytes.insert(0, b'\n');
            }
        }
        if let Err(source) = file.write_all(&bytes).and_then(|()| file.sync_data()) {
            // 本进程写失败时回滚尾部；无法回滚则下次读校验显式报错，绝不作为静默成功。
            let _ = file.set_len(len);
            return Err(ScheduleStoreError::Io { path, source });
        }
        Ok(())
    }

    fn same_run(a: &ScheduleRunRecord, b: &ScheduleRunRecord) -> bool {
        match (a.task_id, b.task_id) {
            (Some(a), Some(b)) => a == b,
            (None, None) => a.fired_at == b.fired_at && a.outcome == b.outcome,
            _ => false,
        }
    }

    fn read_runs(&self, id: Uuid) -> Result<Vec<ScheduleRunRecord>, ScheduleStoreError> {
        let path = self.schedule_dir(id).join("runs.jsonl");
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(ScheduleStoreError::Io { path, source }),
        };
        let mut records = Vec::new();
        for (line, raw) in BufReader::new(file).lines().enumerate() {
            let raw = raw.map_err(|source| ScheduleStoreError::Io {
                path: path.clone(),
                source,
            })?;
            let record: ScheduleRunRecord =
                serde_json::from_str(&raw).map_err(|source| ScheduleStoreError::LedgerRecord {
                    path: path.clone(),
                    line: line + 1,
                    source,
                })?;
            if record.schedule_id != id {
                return Err(ScheduleStoreError::Io {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("schedule mismatch at line {}", line + 1),
                    ),
                });
            }
            if !records.iter().any(|old| Self::same_run(old, &record)) {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub fn run_summary(
        &self,
        id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<RunSummary, ScheduleStoreError> {
        let mut summary = RunSummary::default();
        for record in self
            .read_runs(id)?
            .into_iter()
            .filter(|record| record.fired_at >= since)
        {
            summary.total += 1;
            match record.outcome {
                RunOutcome::Notified => summary.notified += 1,
                RunOutcome::Failed => summary.failed += 1,
                RunOutcome::SkippedOverlap => summary.skipped += 1,
                RunOutcome::Missed => summary.missed += 1,
                RunOutcome::Silent => {}
            }
        }
        Ok(summary)
    }

    /// 读取全部定义。文件不存在返回空列表；内容损坏时把原文件改名为
    /// `<name>.corrupt-<时间戳>` 并返回空列表。
    pub fn load(&self) -> Result<Vec<ScheduleDefinition>, ScheduleStoreError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ScheduleStoreError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        match serde_json::from_str::<StoreFile>(&raw) {
            Ok(file) => {
                debug!(
                    event = "ScheduleStoreLoaded",
                    path = %self.path.display(),
                    count = file.schedules.len(),
                    "loaded scheduled task definitions"
                );
                Ok(file.schedules)
            }
            Err(e) => {
                let backup = self.corrupt_backup_path();
                warn!(
                    event = "ScheduleStoreCorrupted",
                    path = %self.path.display(),
                    backup = %backup.display(),
                    error = %e,
                    "schedule store unreadable, moved aside and starting empty"
                );
                fs::rename(&self.path, &backup).map_err(|source| ScheduleStoreError::Io {
                    path: backup,
                    source,
                })?;
                Ok(Vec::new())
            }
        }
    }

    /// 原子写入全部定义。
    pub fn save(&self, schedules: &[ScheduleDefinition]) -> Result<(), ScheduleStoreError> {
        let io_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| ScheduleStoreError::Io { path, source }
        };
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let content = serde_json::to_string_pretty(&StoreFile {
            version: STORE_VERSION,
            schedules: schedules.to_vec(),
        })?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, content).map_err(io_err(&tmp))?;
        fs::rename(&tmp, &self.path).map_err(io_err(&self.path))?;
        debug!(
            event = "ScheduleStoreSaved",
            path = %self.path.display(),
            count = schedules.len(),
            "saved scheduled task definitions"
        );
        Ok(())
    }

    fn corrupt_backup_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "schedules.json".to_string());
        self.path.with_file_name(format!(
            "{name}.corrupt-{}",
            Utc::now().format("%Y%m%dT%H%M%S")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ScheduleExpr;
    use uuid::Uuid;

    fn definition(name: &str, schedule: &str) -> ScheduleDefinition {
        ScheduleDefinition::new(
            Uuid::new_v4(),
            Some(name.to_string()),
            format!("content of {name}"),
            ScheduleExpr::parse(schedule).unwrap(),
            None,
            Utc::now(),
        )
    }

    fn store_in(dir: &tempfile::TempDir) -> ScheduleStore {
        ScheduleStore::new(dir.path().join("nested/schedules.json"))
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store_in(&dir).load().unwrap().is_empty());
    }

    fn run_record(id: Uuid, outcome: RunOutcome, fired_at: DateTime<Utc>) -> ScheduleRunRecord {
        ScheduleRunRecord {
            task_id: Some(crate::domain::TaskId::new()),
            schedule_id: id,
            fired_at,
            finished_at: fired_at + chrono::Duration::seconds(1),
            outcome,
            error: None,
            result_summary: "checked".into(),
            notification_count: u32::from(outcome == RunOutcome::Notified),
        }
    }

    #[test]
    fn run_ledger_persists_outcomes_filters_window_and_keeps_state_directory_separate() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let id = Uuid::new_v4();
        let now = Utc::now();
        for outcome in [
            RunOutcome::Notified,
            RunOutcome::Silent,
            RunOutcome::Failed,
            RunOutcome::SkippedOverlap,
            RunOutcome::Missed,
        ] {
            store.append_run(&run_record(id, outcome, now)).unwrap();
        }
        store
            .append_run(&run_record(
                id,
                RunOutcome::Failed,
                now - chrono::Duration::days(2),
            ))
            .unwrap();
        let reopened = ScheduleStore::new(store.path());
        let summary = reopened
            .run_summary(id, now - chrono::Duration::hours(24))
            .unwrap();
        assert_eq!(
            summary,
            RunSummary {
                total: 5,
                notified: 1,
                failed: 1,
                skipped: 1,
                missed: 1
            }
        );
        assert_eq!(
            store.state_dir(id),
            store
                .path()
                .parent()
                .unwrap()
                .join(id.to_string())
                .join("state")
        );
        assert_eq!(
            fs::read_to_string(store.schedule_dir(id).join("runs.jsonl"))
                .unwrap()
                .lines()
                .count(),
            6
        );
    }

    #[test]
    fn run_ledger_retries_are_idempotent_for_task_and_non_task_triggers() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let id = Uuid::new_v4();
        let now = Utc::now();
        let task_run = run_record(id, RunOutcome::Silent, now);
        let mut skipped = run_record(id, RunOutcome::SkippedOverlap, now);
        skipped.task_id = None;
        for _ in 0..3 {
            store.append_run(&task_run).unwrap();
            store.append_run(&skipped).unwrap();
        }
        assert_eq!(
            store
                .run_summary(id, now - chrono::Duration::seconds(1))
                .unwrap()
                .total,
            2
        );
    }

    #[test]
    fn run_ledger_io_and_corruption_errors_retain_path_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let id = Uuid::new_v4();
        let path = store.schedule_dir(id).join("runs.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not-json\n").unwrap();
        let error = store.run_summary(id, Utc::now()).unwrap_err().to_string();
        assert!(
            error.contains(&path.display().to_string()) && error.contains("line 1"),
            "{error}"
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let error = store
            .append_run(&run_record(id, RunOutcome::Failed, Utc::now()))
            .unwrap_err()
            .to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");
    }

    #[test]
    fn save_then_load_roundtrips_cron_and_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let mut daily = definition("daily", "cron:0 8 * * *");
        daily.enabled = false;
        daily.health.last_fired_at = Some(Utc::now());
        let once = definition("once", "once:2030-01-02T08:00:00+08:00");

        store.save(&[daily.clone(), once.clone()]).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, daily.id);
        assert_eq!(loaded[0].name, "daily");
        assert_eq!(loaded[0].schedule.raw(), "cron:0 8 * * *");
        assert!(!loaded[0].enabled);
        assert_eq!(loaded[0].health, daily.health);
        assert_eq!(loaded[1].schedule.raw(), once.schedule.raw());
        assert!(loaded[1].schedule.is_once());
        assert!(
            !store.path().with_extension("json.tmp").exists(),
            "temporary file must be renamed away"
        );
    }

    #[test]
    fn save_overwrites_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.save(&[definition("a", "cron:0 8 * * *")]).unwrap();
        store.save(&[]).unwrap();
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "{not json").unwrap();

        assert!(store.load().unwrap().is_empty());
        assert!(!store.path().exists(), "corrupt file must be moved aside");
        let backups: Vec<_> = fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1);
    }
    #[test]
    fn duplicate_physical_records_and_missing_final_newline_do_not_inflate_summary() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let id = Uuid::new_v4();
        let record = run_record(id, RunOutcome::Silent, Utc::now());
        let path = store.schedule_dir(id).join("runs.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = serde_json::to_string(&record).unwrap();
        fs::write(&path, format!("{line}\n{line}")).unwrap();
        store
            .append_run(&run_record(id, RunOutcome::Failed, Utc::now()))
            .unwrap();
        assert_eq!(
            store
                .run_summary(id, DateTime::<Utc>::MIN_UTC)
                .unwrap()
                .total,
            2
        );
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 3);
    }

    #[test]
    fn corrupt_ledger_is_preserved_and_append_refuses_to_hide_missing_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        let id = Uuid::new_v4();
        let record = run_record(id, RunOutcome::Silent, Utc::now());
        store.append_run(&record).unwrap();
        let path = store.schedule_dir(id).join("runs.jsonl");
        let raw = format!("{}{{partial", fs::read_to_string(&path).unwrap());
        fs::write(&path, &raw).unwrap();
        assert!(store.append_run(&record).is_err());
        assert!(store.run_summary(id, DateTime::<Utc>::MIN_UTC).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), raw);
    }
}
