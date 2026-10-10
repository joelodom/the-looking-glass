use crate::imageops::RectF;
use serde::{Deserialize, Serialize};

/// How a node was reached from its parent. Every view change is a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transition {
    /// The starting photo (from disk or "I'm feeling lucky").
    Root,
    /// Zoom into a rectangle of the parent frame, normalized 0..1.
    ZoomIn { rect: RectF },
    /// Widen the view: the parent frame becomes the centered 1/factor region.
    ZoomOut { factor: f32 },
    /// Absolute camera orientation change at the same position.
    Look { tilt_deg: f32, heading_deg: f32 },
    /// A step forward along the view direction. If a wall, door or similar
    /// barrier fills most of the frame, the step passes through it and the
    /// new frame shows the other side; otherwise about a meter closer.
    Forward,
}

impl Transition {
    /// Discrete transitions are repeatable exactly and are reused when the
    /// same one is requested again from the same node.
    pub fn is_discrete(&self) -> bool {
        !matches!(self, Transition::ZoomIn { .. } | Transition::Root)
    }

    /// Equality tolerant of float noise, used for reuse lookups.
    pub fn approx_eq(&self, other: &Transition) -> bool {
        const EPS: f32 = 0.01;
        match (self, other) {
            (Transition::Root, Transition::Root) => true,
            (Transition::ZoomIn { rect: a }, Transition::ZoomIn { rect: b }) => a.approx_eq(b, 0.002),
            (Transition::ZoomOut { factor: a }, Transition::ZoomOut { factor: b }) => (a - b).abs() < EPS,
            (
                Transition::Look { tilt_deg: t1, heading_deg: h1 },
                Transition::Look { tilt_deg: t2, heading_deg: h2 },
            ) => (t1 - t2).abs() < EPS && heading_diff(*h1, *h2).abs() < EPS,
            (Transition::Forward, Transition::Forward) => true,
            _ => false,
        }
    }

    /// Short label for breadcrumbs and outlines.
    pub fn label(&self) -> String {
        match self {
            Transition::Root => "start".into(),
            Transition::ZoomIn { rect } => format!("zoom in ×{:.1}", 1.0 / rect.w.max(1e-6)),
            Transition::ZoomOut { factor } => format!("zoom out ×{:.0}", factor),
            Transition::Look { tilt_deg, heading_deg } => {
                format!("look tilt {:.0}° heading {:.0}°", tilt_deg, heading_deg)
            }
            Transition::Forward => "step forward".into(),
        }
    }
}

/// Signed smallest difference between two headings in degrees.
pub fn heading_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 {
        d - 360.0
    } else {
        d
    }
}

/// Normalize a heading into [0, 360).
pub fn wrap_heading(h: f32) -> f32 {
    h.rem_euclid(360.0)
}
