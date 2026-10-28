//! Session backend implementations

pub mod native;

pub use native::NativeProcessBackend;

#[cfg(unix)]
pub mod supervisor;
