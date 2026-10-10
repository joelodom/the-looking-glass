use crate::camera::Camera;
use crate::log::{CallLog, LogEvent};
use crate::node::AncestorBrief;
use crate::scene::SceneSpec;
use crate::settings::{Quality, Settings};
use crate::transition::Transition;
use image::RgbaImage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared by every service call: a cancel flag the caller may set at any
/// time, a log sink for prompts and replies, and an optional directory where
/// the service should write a record of the call.
#[derive(Clone)]
pub struct CallContext {
    pub cancel: Arc<AtomicBool>,
    pub log: Arc<dyn CallLog>,
    /// e.g. `<world dir>/calls`. Services write `<purpose>-<timestamp>.json`
    /// records here when present.
    pub record_dir: Option<PathBuf>,
}

impl CallContext {
    pub fn new(log: Arc<dyn CallLog>) -> Self {
        Self { cancel: Arc::new(AtomicBool::new(false)), log, record_dir: None }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn log(&self, event: LogEvent) {
        self.log.event(&event);
    }
}

/// What kind of describe call this is.
#[derive(Debug, Clone, PartialEq)]
pub enum DescribeKind {
    /// Analyse the starting photo: guess place, scale, time, season. No
    /// render prompt is needed (the pipeline ignores it) but the camera,
    /// title and dossier are.
    Root,
    /// Invent a "feeling lucky" starting photo: pick a real, named place on
    /// Earth at random and a random vantage (ground or aerial). The render
    /// prompt describes that photo.
    Lucky,
    /// A view change from the current frame.
    Step,
    /// The image model refused `previous`; rewrite the prompt once in more
    /// generic, safer terms.
    Rewrite { refused_message: String, previous: SceneSpec },
}

#[derive(Debug, Clone)]
pub struct DescribeRequest {
    pub kind: DescribeKind,
    /// The transition being taken (Root for Root/Lucky).
    pub transition: Transition,
    /// Current frame (the one being left), absolute path. None for Lucky.
    pub current_image: Option<PathBuf>,
    /// For ZoomIn: the crop, upscaled to frame size, absolute path.
    pub crop_image: Option<PathBuf>,
    /// The current frame's parent, when one exists, absolute path.
    pub parent_image: Option<PathBuf>,
    /// The current frame's spec (dossier lives here).
    pub prior_spec: Option<SceneSpec>,
    /// The camera after applying the transition arithmetically. Claude
    /// starts from this and may adjust (e.g. regime changes, scale
    /// compression into the imaginary).
    pub proposed_camera: Option<Camera>,
    /// Root-first chain of ancestors, excluding the current node.
    pub ancestry: Vec<AncestorBrief>,
    /// Directory the images live in; used as the child's working directory.
    pub world_dir: PathBuf,
    pub settings: Settings,
}

#[derive(Debug, thiserror::Error)]
pub enum DescribeError {
    #[error("cancelled")]
    Cancelled,
    #[error("transient: {0}")]
    Transient(String),
    #[error("could not parse the reply: {0}")]
    Parse(String),
    #[error("{0}")]
    Fatal(String),
}

/// The reasoning half. Implemented over the headless claude CLI, and by a mock.
pub trait Describer: Send + Sync {
    fn describe(&self, req: &DescribeRequest, ctx: &CallContext) -> Result<SceneSpec, DescribeError>;
}

/// One image call. Routing rule for the OpenAI implementation:
/// - `canvas` Some: edits endpoint, `image[0]` = canvas, `mask` if Some,
///   `image[1..]` = references.
/// - `canvas` None, references non-empty: edits endpoint, `image[..]` =
///   references, no mask.
/// - `canvas` None, references empty: generations endpoint.
#[derive(Clone)]
pub struct PaintRequest {
    pub model: String,
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    pub quality: Quality,
    pub canvas: Option<RgbaImage>,
    /// Same size as the canvas. Alpha 0 = paint here, 255 = keep.
    pub mask: Option<RgbaImage>,
    pub references: Vec<RgbaImage>,
    /// For the ledger and the stdout log.
    pub purpose: String,
}

#[derive(Debug, Clone)]
pub struct PaintResult {
    pub image: RgbaImage,
    /// The model that actually answered (may differ after a fallback).
    pub model_used: String,
    pub usage: Option<serde_json::Value>,
    pub estimated_usd: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum PaintError {
    #[error("cancelled")]
    Cancelled,
    /// Moderation / content policy. The pipeline asks Claude to rewrite once.
    #[error("refused: {0}")]
    Refused(String),
    #[error("transient: {0}")]
    Transient(String),
    #[error("no usable image model: {0}")]
    ModelUnavailable(String),
    #[error("{0}")]
    Fatal(String),
}

/// The painting half. Implemented over the OpenAI images API, and by a mock.
pub trait Painter: Send + Sync {
    fn paint(&self, req: &PaintRequest, ctx: &CallContext) -> Result<PaintResult, PaintError>;
}
