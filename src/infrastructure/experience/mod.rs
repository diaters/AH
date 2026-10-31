//! 经验库文件存储
//!
//! 每条经验一个 `<id>.md` 文件，便于用户直接阅读和修改；
//! 写入经临时文件 + rename 保证原子性，写操作之间由互斥锁串行化。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use tracing::warn;

use crate::contracts::ExperienceLibrary;
use crate::domain::{ExperienceEntry, is_valid_experience_id};

pub const DEFAULT_EXPERIENCE_DIR: &str = ".harness/experience";

pub struct FileExperienceLibrary {
    dir: PathBuf,
    write_lock: Mutex<()>,
}

impl FileExperienceLibrary {
    /// 目录在首次写入时按需创建。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if !is_valid_experience_id(id) {
            bail!("invalid experience id");
        }
        Ok(self.dir.join(format!("{id}.md")))
    }
}

impl Default for FileExperienceLibrary {
    fn default() -> Self {
        Self::new(DEFAULT_EXPERIENCE_DIR)
    }
}

impl ExperienceLibrary for FileExperienceLibrary {
    fn list(&self) -> Result<Vec<ExperienceEntry>> {
        let read_dir = match fs::read_dir(&self.dir) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read experience dir {}", self.dir.display()));
            }
        };
        let mut entries = Vec::new();
        for item in read_dir {
            let path = item?.path();
            let Some(id) = path
                .extension()
                .filter(|ext| *ext == "md")
                .and_then(|_| path.file_stem())
                .and_then(|stem| stem.to_str())
                .filter(|id| is_valid_experience_id(id))
            else {
                continue;
            };
            match fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|text| ExperienceEntry::from_markdown(id, &text).map_err(Into::into))
            {
                Ok(entry) => entries.push(entry),
                Err(error) => warn!(
                    event = "ExperienceFileSkipped",
                    path = %path.display(),
                    error = %error,
                    "skipping unreadable experience file"
                ),
            }
        }
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        Ok(entries)
    }

    fn get(&self, id: &str) -> Result<Option<ExperienceEntry>> {
        let path = self.path(id)?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(ExperienceEntry::from_markdown(id, &text)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    fn save(&self, entry: &ExperienceEntry) -> Result<()> {
        entry.validate()?;
        let path = self.path(&entry.id)?;
        let _guard = self
            .write_lock
            .lock()
            .expect("experience write lock poisoned");
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("create experience dir {}", self.dir.display()))?;
        let tmp = self.dir.join(format!(".{}.md.tmp", entry.id));
        fs::write(&tmp, entry.to_markdown()).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<bool> {
        let path = self.path(id)?;
        let _guard = self
            .write_lock
            .lock()
            .expect("experience write lock poisoned");
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("delete {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str) -> ExperienceEntry {
        ExperienceEntry::new(title, "## 适用场景\n场景", None, chrono::Utc::now()).unwrap()
    }

    #[test]
    fn save_list_get_delete_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let library = FileExperienceLibrary::new(tmp.path().join("exp"));
        assert!(library.list().unwrap().is_empty());

        let first = entry("第一条");
        library.save(&first).unwrap();
        assert_eq!(library.get(&first.id).unwrap(), Some(first.clone()));
        assert_eq!(library.list().unwrap(), vec![first.clone()]);

        assert!(library.delete(&first.id).unwrap());
        assert!(!library.delete(&first.id).unwrap());
        assert_eq!(library.get(&first.id).unwrap(), None);
    }

    #[test]
    fn list_skips_malformed_and_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let library = FileExperienceLibrary::new(tmp.path());
        let good = entry("好的");
        library.save(&good).unwrap();
        fs::write(
            tmp.path().join(format!("{}.md", uuid::Uuid::new_v4())),
            "no front matter",
        )
        .unwrap();
        fs::write(tmp.path().join("notes.md"), "+++\n+++\n").unwrap();
        assert_eq!(library.list().unwrap(), vec![good]);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let library = FileExperienceLibrary::new(tmp.path());
        assert!(library.get("../secret").is_err());
        assert!(library.delete("../secret").is_err());
    }
}
