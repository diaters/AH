//! 附件始终通过授权 ID 访问，客户端不能提供服务器路径。
use super::super::ClientError;
use super::{Store, append_event, hash};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};
use uuid::Uuid;

pub(crate) const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
/// 原机已授权产物（如短视频）的上限，高于设备上传上限。
pub(crate) const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientFile {
    pub id: Uuid,
    pub user_id: String,
    pub session_id: Uuid,
    pub name: String,
    pub size: usize,
    pub sha256: String,
    pub kind: String,
    pub source: String,
}
/// 客户端据此选择预览方式；无法识别的类型只提供保存。
pub(crate) fn file_kind(name: &str) -> &'static str {
    if crate::domain::image_mime_type(name).is_some() {
        return "image";
    }
    match extension(name).to_ascii_lowercase().as_str() {
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "ts" => "video",
        "pdf" => "pdf",
        "html" | "htm" => "html",
        "txt" | "md" | "markdown" | "log" | "csv" | "tsv" | "json" | "jsonl" | "toml" | "yaml"
        | "yml" | "xml" | "ini" | "conf" | "rs" | "py" | "js" | "tsx" | "jsx" | "ets" | "sh"
        | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "java" | "kt" | "swift" | "sql" | "css"
        | "diff" | "patch" => "text",
        _ => "document",
    }
}
fn extension(name: &str) -> &str {
    std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .filter(|s| s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("bin")
}
impl Store {
    fn file_path(&self, file: &ClientFile) -> PathBuf {
        self.path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("client-files")
            .join(format!("{}.{}", file.id, extension(&file.name)))
    }
    pub(crate) fn file(&self, user: &str, id: Uuid) -> Result<ClientFile, ClientError> {
        self.state
            .files
            .get(&id)
            .filter(|f| f.user_id == user)
            .cloned()
            .ok_or(ClientError::NotFound)
    }
    pub(crate) fn read_file(
        &self,
        user: &str,
        id: Uuid,
    ) -> Result<(ClientFile, Vec<u8>), ClientError> {
        let file = self.file(user, id)?;
        let data = fs::File::open(self.file_path(&file))?;
        let mut bytes = vec![];
        data.take((file.size + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() != file.size || format!("{:x}", Sha256::digest(&bytes)) != file.sha256 {
            return Err(ClientError::StorageUnavailable);
        }
        Ok((file, bytes))
    }
    pub(crate) fn store_file(
        &mut self,
        user: &str,
        session: Uuid,
        id: Uuid,
        name: &str,
        bytes: &[u8],
        source: &str,
    ) -> Result<ClientFile, ClientError> {
        if self.failed {
            return Err(ClientError::StorageUnavailable);
        }
        self.session(user, session)?;
        if name.is_empty()
            || name.len() > 255
            || name.contains(['/', '\\'])
            || name.chars().any(char::is_control)
            || matches!(name, "." | "..")
        {
            return Err(ClientError::Invalid("invalid file name"));
        }
        let limit = if source == "artifact" {
            MAX_ARTIFACT_BYTES
        } else {
            MAX_FILE_BYTES
        };
        if bytes.len() > limit {
            return Err(ClientError::Invalid("file too large"));
        }
        let image = crate::domain::image_mime_type(name).is_some();
        if image && bytes.len() > crate::domain::MAX_IMAGE_BYTES as usize {
            return Err(ClientError::Invalid("image too large"));
        }
        let digest = format!("{:x}", Sha256::digest(bytes));
        if let Some(old) = self.state.files.get(&id) {
            if old.user_id != user
                || old.session_id != session
                || old.name != name
                || old.sha256 != digest
            {
                return Err(ClientError::Conflict("file ID already used"));
            }
            return self.read_file(user, id).map(|(file, _)| file);
        }
        if self.state.files.len() >= 256 {
            return Err(ClientError::Capacity);
        }
        let file = ClientFile {
            id,
            user_id: user.into(),
            session_id: session,
            name: name.into(),
            size: bytes.len(),
            sha256: digest,
            kind: file_kind(name).into(),
            source: source.into(),
        };
        let path = self.file_path(&file);
        let parent = path.parent().unwrap();
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(parent)?;
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut output) => {
                if let Err(error) = output.write_all(bytes).and_then(|_| output.sync_all()) {
                    let _ = fs::remove_file(&path);
                    return Err(error.into());
                }
            }
            // 进程可能在数据落盘后、元数据提交前退出。只恢复完全相同的原文件。
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&path)?.is_file() {
                    return Err(ClientError::StorageUnavailable);
                }
                let input = fs::File::open(&path)?;
                let mut existing = vec![];
                input.take((limit + 1) as u64).read_to_end(&mut existing)?;
                if existing != bytes {
                    return Err(ClientError::Conflict("unregistered file content differs"));
                }
                fs::File::open(&path)?.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        // 状态提交失败可能发生在 rename 后；保留数据以免损坏已提交记录。
        self.change(|state| {
            state.files.insert(id, file.clone());
            append_event(state, session, "file_added", serde_json::to_value(&file)?);
            Ok(file)
        })
    }
    pub(crate) fn file_markers(
        &self,
        user: &str,
        session: Uuid,
        ids: &[Uuid],
    ) -> Result<String, ClientError> {
        if ids.len() > 8 {
            return Err(ClientError::Invalid("too many attachments"));
        }
        let mut markers = String::new();
        for id in ids {
            let (file, _) = self.read_file(user, *id)?;
            if file.session_id != session {
                return Err(ClientError::NotFound);
            }
            let path = fs::canonicalize(self.file_path(&file))?;
            let path = path
                .to_str()
                .filter(|p| !p.contains(['[', ']']) && !p.chars().any(char::is_control))
                .ok_or(ClientError::Invalid(
                    "attachment storage path cannot be represented",
                ))?;
            let kind = if file.kind == "image" {
                "IMAGE"
            } else {
                "DOCUMENT"
            };
            markers.push_str(&format!("\n附件 {id}\n[{kind}:{path}]"));
        }
        Ok(markers)
    }
    pub(crate) fn import_attachment(
        &mut self,
        user: &str,
        session: Uuid,
        target: &str,
    ) -> Result<ClientFile, ClientError> {
        let target = target.strip_prefix("file://").unwrap_or(target);
        if target.starts_with("http:") || target.starts_with("https:") {
            return Err(ClientError::Invalid("remote attachment URL is unsupported"));
        }
        let path = std::path::Path::new(target);
        let input = fs::File::open(path)?;
        if !input.metadata()?.is_file() {
            return Err(ClientError::Invalid("artifact is not a regular file"));
        }
        let mut bytes = vec![];
        input
            .take((MAX_ARTIFACT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("artifact.bin");
        // 同一已授权出向文件重试不重复注册；不公开源路径或其摘要。
        let key = format!(
            "{user}:{session}:{target}:{}",
            hash(&format!("{:x}", Sha256::digest(&bytes)))
        );
        let digest = Sha256::digest(key.as_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&digest[..16]);
        self.store_file(
            user,
            session,
            Uuid::from_bytes(id),
            name,
            &bytes,
            "artifact",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_select_viewer_by_extension() {
        assert_eq!(file_kind("shot.PNG"), "image");
        assert_eq!(file_kind("demo.mp4"), "video");
        assert_eq!(file_kind("report.pdf"), "pdf");
        assert_eq!(file_kind("index.html"), "html");
        assert_eq!(file_kind("README.md"), "text");
        assert_eq!(file_kind("main.rs"), "text");
        assert_eq!(file_kind("archive.zip"), "document");
        assert_eq!(file_kind("noext"), "document");
    }
}
