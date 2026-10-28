//! JSON 文件定时任务存储
//!
//! 单文件保存全部定义，写入走"临时文件 + rename"原子替换；文件损坏时改名备份后按空处理，
//! 避免一次坏写阻断进程启动。

use std::fs;
use std::path::{Path, PathBuf};

use bevy_ecs::prelude::Resource;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::domain::ScheduleDefinition;

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
}
