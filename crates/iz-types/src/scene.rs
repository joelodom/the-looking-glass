use crate::camera::Camera;
use serde::{Deserialize, Serialize};

/// For ZoomIn only: whether the upscaled crop is passed as the image to edit
/// (small jumps, under about 3x) or only as a reference (large jumps).
/// The pipeline forces outpainting for ZoomOut and reference mode for Look
/// and Forward regardless of this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CanvasMode {
    Crop,
    #[default]
    Reference,
}

/// What Claude returns for every describe call. This is the contract between
/// the reasoning model and the painting model; the dossier is what keeps a
/// branch consistent with itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneSpec {
    /// Short title for the frame, e.g. "Times Square from 300 m".
    pub title: String,
    /// The full prompt sent to the image model. Self-contained: style,
    /// camera, lighting, content, what to keep from the references.
    pub render_prompt: String,
    /// Claude's estimate of the camera for this frame.
    pub camera: Camera,
    /// See [`CanvasMode`]. Only consulted for ZoomIn.
    #[serde(default)]
    pub canvas_mode: CanvasMode,
    /// Running world knowledge: location, lat/lon if known, time of day,
    /// season, weather, established facts about what has been drawn so far.
    /// Free-form JSON object owned by Claude and passed back verbatim.
    #[serde(default)]
    pub dossier: serde_json::Value,
    /// Anything Claude wants the user (stdout) to know: uncertainty, the
    /// regime switch it made, a substitution for safety.
    #[serde(default)]
    pub notes: String,
}

impl SceneSpec {
    /// JSON Schema for the structured output. Kept here so the describer and
    /// the mocks agree on the shape.
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["title", "render_prompt", "camera", "canvas_mode", "dossier", "notes"],
            "properties": {
                "title": { "type": "string" },
                "render_prompt": { "type": "string" },
                "camera": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["frame_width_m", "distance_m", "tilt_deg", "heading_deg", "regime", "scale_label"],
                    "properties": {
                        "frame_width_m": { "type": "number" },
                        "distance_m": { "type": "number" },
                        "tilt_deg": { "type": "number" },
                        "heading_deg": { "type": "number" },
                        "regime": { "type": "string" },
                        "scale_label": { "type": "string" }
                    }
                },
                "canvas_mode": { "type": "string", "enum": ["crop", "reference"] },
                "dossier": { "type": "object" },
                "notes": { "type": "string" }
            }
        })
    }
}
