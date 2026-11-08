//! 契约层
//!
//! 定义模块间的稳定接口，支撑模块替换和测试。

mod experience;
mod memory;
mod runtime;
mod transcription;
mod usage;
mod wake;

pub use experience::{ExperienceLibrary, SharedExperienceLibrary};
pub use memory::MemoryStore;
pub use runtime::{AsyncRuntime, BufferedIngress, Clock, FrontendRegistry};
#[cfg(test)]
pub(crate) use transcription::FakeTranscriber;
pub use transcription::{
    SharedVoiceTranscriber, TranscriptionError, VoiceTranscriber, render_transcription_outcomes,
};
pub use usage::{SharedUsageLedger, UsageLedger};
pub use wake::{
    ExecutionResultSender, ModelChainStateUpdateSender, RuntimeDeadlines, RuntimeWake,
    ToolResultSender, WakeSender,
};
