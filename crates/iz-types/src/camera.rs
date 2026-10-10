use serde::{Deserialize, Serialize};

/// Where the virtual camera is and what it can see. Claude estimates these;
/// the pipeline adjusts them for each transition before asking again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Width of the visible frame, in meters, at the subject. The core scale
    /// quantity: a zoom into a rect of width fraction `f` multiplies it by `f`;
    /// a zoom out by factor `k` multiplies it by `k`. Can be astronomically
    /// large or far below a nanometer.
    pub frame_width_m: f64,
    /// Distance from the camera to the subject, in meters.
    pub distance_m: f64,
    /// 0 = looking straight down, 90 = looking at the horizon, 180 = straight up.
    pub tilt_deg: f32,
    /// Compass heading of the view direction, 0 = north, 90 = east.
    pub heading_deg: f32,
    /// Free text chosen by Claude from a suggested vocabulary:
    /// cosmic, planetary, geographic, street, human, eye, cellular,
    /// molecular, imaginary. The UI displays it; nothing enforces it.
    pub regime: String,
    /// Short human-readable scale, e.g. "about 40 km across", "2 mm across".
    pub scale_label: String,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            frame_width_m: 100.0,
            distance_m: 100.0,
            tilt_deg: 0.0,
            heading_deg: 0.0,
            regime: "geographic".to_string(),
            scale_label: String::new(),
        }
    }
}

impl Camera {
    /// True when the camera looks (nearly) straight down.
    pub fn is_top_down(&self) -> bool {
        self.tilt_deg.abs() < 1.0
    }
}
