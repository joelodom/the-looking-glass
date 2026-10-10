//! Shared contract for The Looking Glass workspace: types, traits, settings,
//! image operations and logging. Every other crate depends only on this one.

pub mod camera;
pub mod engine;
pub mod imageops;
pub mod log;
pub mod node;
pub mod scene;
pub mod settings;
pub mod traits;
pub mod transition;

pub use camera::Camera;
pub use engine::{Action, BusyInfo, Engine, EngineError, EngineEvent, Outcome, Service, StatusEvent};
pub use imageops::RectF;
pub use log::{CallLog, LogEvent, LogKind, MultiLog, StdoutLog};
pub use node::{AncestorBrief, Backlink, ChildOutline, Node, NodeId, World, WorldId, WorldSummary, WorldView};
pub use scene::{CanvasMode, SceneSpec};
pub use settings::{Quality, Settings};
pub use traits::{
    CallContext, DescribeError, DescribeKind, DescribeRequest, Describer, PaintError,
    PaintRequest, PaintResult, Painter,
};
pub use transition::Transition;

/// The one place the product name lives. Renaming the project is a change here
/// (plus the binary name in `crates/iz-app/Cargo.toml`).
pub const APP_NAME: &str = "The Looking Glass";
/// Folder name under the platform data directory (no spaces).
pub const APP_DIR_NAME: &str = "TheLookingGlass";

/// Environment variable names understood by the app.
pub mod env {
    /// OpenAI API key (also read from `.env`).
    pub const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
    /// Set to `1` to use the mock services (no network, no cost).
    pub const MOCK: &str = "IZ_MOCK";
    /// Path to the `claude` binary, overriding discovery.
    pub const CLAUDE_BIN: &str = "IZ_CLAUDE_BIN";
    /// Overrides the app data directory (worlds, settings, ledger). Tests
    /// point this at a temp dir.
    pub const DATA_DIR: &str = "IZ_DATA_DIR";
    /// Set to `1` to let a crate's tests make real service calls.
    pub const REAL_TESTS: &str = "IZ_REAL_TESTS";
}
