use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogKind {
    Request,
    Response,
    Note,
    Error,
}

/// A prompt, a reply, or a note about a service call. Image data never goes
/// in `text`; file paths do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    /// "claude", "openai", "local", "mock"
    pub service: String,
    /// e.g. "describe zoom in", "paint zoom out", "lucky"
    pub purpose: String,
    pub kind: LogKind,
    pub text: String,
}

impl LogEvent {
    pub fn new(service: &str, purpose: &str, kind: LogKind, text: impl Into<String>) -> Self {
        Self { service: service.into(), purpose: purpose.into(), kind, text: text.into() }
    }
}

pub trait CallLog: Send + Sync {
    fn event(&self, event: &LogEvent);
}

/// Prints every event to stdout, framed so prompts and replies are easy to
/// find in a scrollback.
pub struct StdoutLog;

impl CallLog for StdoutLog {
    fn event(&self, e: &LogEvent) {
        let kind = match e.kind {
            LogKind::Request => "REQUEST",
            LogKind::Response => "RESPONSE",
            LogKind::Note => "NOTE",
            LogKind::Error => "ERROR",
        };
        let stamp = chrono::Local::now().format("%H:%M:%S");
        println!("\n===== [{stamp}] {} · {} · {kind} =====", e.service, e.purpose);
        println!("{}", e.text);
        println!("===== end {kind} =====");
    }
}

/// Fans an event out to several sinks (stdout plus a per-world file, say).
pub struct MultiLog(pub Vec<std::sync::Arc<dyn CallLog>>);

impl CallLog for MultiLog {
    fn event(&self, event: &LogEvent) {
        for sink in &self.0 {
            sink.event(event);
        }
    }
}
