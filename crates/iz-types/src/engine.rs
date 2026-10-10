use crate::imageops::RectF;
use crate::node::{NodeId, WorldId, WorldSummary, WorldView};
use crate::settings::Settings;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What the UI asks the engine to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    OpenPhoto(PathBuf),
    OpenWorld(WorldId),
    Lucky,
    ZoomIn(RectF),
    ZoomOut,
    /// Relative steps; the engine turns them into an absolute Look.
    Tilt { delta_deg: f32 },
    Rotate { delta_deg: f32 },
    /// Straight down, heading north.
    Home,
    Forward,
    Backward,
    /// History: to the parent of the current node.
    Back,
    /// History: undo the last Back.
    Redo,
    /// Jump to an existing child (clicked outline).
    EnterChild(NodeId),
    /// Re-describe and re-paint the current node; discards its descendants.
    Regenerate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Claude,
    OpenAI,
    Local,
    Mock,
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Service::Claude => "Claude",
            Service::OpenAI => "OpenAI",
            Service::Local => "local",
            Service::Mock => "mock",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Cancelled,
    Refused,
    Failed(String),
}

/// Progress of the call in flight, for the status bar.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusEvent {
    Started { service: Service, purpose: String, at: Instant },
    Finished { service: Service, purpose: String, elapsed: Duration, outcome: Outcome },
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Status(StatusEvent),
    /// The view changed (new node, back, regenerate...). Call `view()`.
    ViewChanged,
    /// A world was opened or created; the open screen can be left.
    WorldOpened,
    /// The saved-worlds list changed.
    WorldsChanged,
    /// Shown in the status bar; also logged.
    Info(String),
    /// Shown in the status bar; also logged.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BusyInfo {
    pub service: Service,
    pub purpose: String,
    pub since: Instant,
    /// Which step of the action this is, e.g. "1/2 describe".
    pub step: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("a generation is already running")]
    Busy,
    #[error("no world is open")]
    NoWorld,
    #[error("{0}")]
    Other(String),
}

/// The UI's only handle on the world and the services. Non-blocking: work
/// happens on a background thread, results arrive via `poll`.
pub trait Engine: Send {
    /// Start an action. `settings` is a snapshot taken at submit time.
    fn submit(&mut self, action: Action, settings: Settings) -> Result<(), EngineError>;
    /// Cancel the running action, if any. A cancelled image call is still
    /// billed by the provider; the result is discarded.
    fn cancel(&mut self);
    /// Drain events since the last poll.
    fn poll(&mut self) -> Vec<EngineEvent>;
    /// Current view, None before a world is open.
    fn view(&self) -> Option<WorldView>;
    /// Saved worlds, most recently updated first.
    fn list_worlds(&self) -> Vec<WorldSummary>;
    /// The call in flight, if any.
    fn busy(&self) -> Option<BusyInfo>;
}
