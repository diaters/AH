//! 专用进程审计日志：每条 JSON 同步整行追加，独立于宿主异步日志线程。

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(super) struct ProcessLog(Arc<Mutex<File>>);

impl ProcessLog {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map(|file| Self(Arc::new(Mutex::new(file))))
    }

    fn append(&self, bytes: &[u8]) {
        let mut file = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = file.write_all(bytes).and_then(|()| file.flush());
    }

    pub(super) fn event(&self, event: &str, mut fields: serde_json::Value) {
        fields["event"] = event.into();
        let value = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "level": "ERROR", "target": "harness::systems::tools::backend::supervisor", "fields": fields,
        });
        if let Ok(mut bytes) = serde_json::to_vec(&value) {
            bytes.push(b'\n');
            self.append(&bytes);
        }
    }
}

/// tracing 的格式化可进行多次 write；在 Drop 时才把整条事件交给追加文件。
pub(super) struct Record {
    log: ProcessLog,
    bytes: Vec<u8>,
}

impl Write for Record {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        self.log.append(&self.bytes);
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ProcessLog {
    type Writer = Record;
    fn make_writer(&'a self) -> Self::Writer {
        Record {
            log: self.clone(),
            bytes: Vec::new(),
        }
    }
}
