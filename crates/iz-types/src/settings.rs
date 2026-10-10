use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Image quality tier passed to the image API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Low,
    Medium,
    #[default]
    High,
    XHigh,
    Max,
    Auto,
}

impl Quality {
    pub const ALL: [Quality; 6] = [
        Quality::Low,
        Quality::Medium,
        Quality::High,
        Quality::XHigh,
        Quality::Max,
        Quality::Auto,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Low => "low",
            Quality::Medium => "medium",
            Quality::High => "high",
            Quality::XHigh => "xhigh",
            Quality::Max => "max",
            Quality::Auto => "auto",
        }
    }
}

/// User-tunable settings, edited on the ESC screen, persisted as JSON.
/// A copy travels with every engine submit so a change applies to the next
/// frame and never races a running one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub image_model: String,
    pub image_quality: Quality,
    pub claude_model: String,
    pub frame_width: u32,
    pub frame_height: u32,
    pub zoom_out_factor: f32,
    pub tilt_step_deg: f32,
    pub heading_step_deg: f32,
    /// Explicit path to the claude binary; None = discover.
    pub claude_bin: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            image_model: "gpt-image-2.5-sunburst".to_string(),
            image_quality: Quality::High,
            claude_model: "claude-opus-5-5".to_string(),
            frame_width: 1536,
            frame_height: 1024,
            zoom_out_factor: 16.0,
            tilt_step_deg: 30.0,
            heading_step_deg: 45.0,
            claude_bin: None,
        }
    }
}

impl Settings {
    pub fn frame_aspect(&self) -> f32 {
        self.frame_width as f32 / self.frame_height.max(1) as f32
    }

    pub fn load() -> Settings {
        let path = paths::settings_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = paths::settings_file();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).expect("settings serialize");
        write_atomic(&path, text.as_bytes())
    }
}

/// Write via a temp file and rename so a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Platform locations. On macOS: ~/Library/Application Support/TheLookingGlass.
pub mod paths {
    use super::PathBuf;
    use crate::APP_DIR_NAME;

    pub fn app_dir() -> PathBuf {
        if let Ok(dir) = std::env::var(crate::env::DATA_DIR) {
            if !dir.is_empty() {
                return PathBuf::from(dir);
            }
        }
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_DIR_NAME)
    }

    pub fn worlds_dir() -> PathBuf {
        app_dir().join("worlds")
    }

    pub fn settings_file() -> PathBuf {
        app_dir().join("settings.json")
    }

    /// JSONL ledger of every image API call.
    pub fn ledger_file() -> PathBuf {
        app_dir().join("image-calls.jsonl")
    }
}

/// Load `.env` files best-effort: the current directory, then the executable's
/// directory and its ancestors (so `cargo run` from a crate dir still finds
/// the workspace `.env`), then the app dir. Existing env vars win.
pub fn load_dotenv() {
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from(".env")];
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            candidates.push(d.join(".env"));
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    candidates.push(paths::app_dir().join(".env"));
    for c in candidates {
        if c.is_file() {
            let _ = dotenvy::from_path(&c);
        }
    }
}

/// True when `IZ_MOCK` is set to something truthy.
pub fn mock_mode() -> bool {
    matches!(
        std::env::var(crate::env::MOCK).as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}
