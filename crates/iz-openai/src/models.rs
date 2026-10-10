//! What each image model accepts: sizes, quality tiers, `input_fidelity`,
//! and the order in which models are tried when one is unavailable.

use iz_types::Quality;

/// Tried in this order (skipping the one just tried) when the requested model
/// does not exist or the account cannot use it.
pub const FALLBACK_MODELS: [&str; 5] = [
    "gpt-image-2.5-sunburst",
    "gpt-image-2.5-flare",
    "gpt-image-2",
    "gpt-image-1.5",
    "gpt-image-1",
];

/// The sizes every GPT image model accepts, and the only ones gpt-image-1 and
/// gpt-image-1.5 accept.
pub const STANDARD_SIZES: [(u32, u32); 3] = [(1024, 1024), (1536, 1024), (1024, 1536)];

/// Custom-size limits for the gpt-image-2 family (2 and 2.5).
pub const SIZE_STEP: u32 = 16;
pub const MAX_EDGE: u32 = 3840;
pub const MIN_PIXELS: u64 = 655_360;
pub const MAX_PIXELS: u64 = 8_294_400;
pub const MAX_ASPECT: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// gpt-image-2.5-sunburst, gpt-image-2.5-flare and their snapshots.
    Gpt25,
    /// gpt-image-2 and its snapshots.
    Gpt2,
    Gpt15,
    Gpt1,
    Gpt1Mini,
    /// Anything else. Treated permissively: custom sizes allowed (with the
    /// gpt-image-2 rules), quality passed through, no `input_fidelity`.
    Unknown,
}

/// `model` is `base` or a dated snapshot of it (`base-2026-04-21`).
fn is_model(model: &str, base: &str) -> bool {
    model
        .strip_prefix(base)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

pub fn family(model: &str) -> Family {
    let model = model.trim();
    if model.starts_with("gpt-image-2.5-") || model == "gpt-image-2.5" {
        Family::Gpt25
    } else if is_model(model, "gpt-image-2") {
        Family::Gpt2
    } else if is_model(model, "gpt-image-1.5") {
        Family::Gpt15
    } else if is_model(model, "gpt-image-1-mini") {
        Family::Gpt1Mini
    } else if is_model(model, "gpt-image-1") {
        Family::Gpt1
    } else {
        Family::Unknown
    }
}

impl Family {
    /// Accepts arbitrary `WxH` within the gpt-image-2 limits.
    pub fn custom_sizes(self) -> bool {
        matches!(self, Family::Gpt25 | Family::Gpt2 | Family::Unknown)
    }

    /// Accepts the `xhigh` and `max` quality tiers.
    pub fn extended_quality(self) -> bool {
        matches!(self, Family::Gpt25 | Family::Unknown)
    }

    /// The `input_fidelity` value to send on the edits endpoint, if any.
    /// Only gpt-image-1 and 1.5 accept "high"; mini accepts only the default
    /// and gpt-image-2+ reject the parameter.
    pub fn input_fidelity(self) -> Option<&'static str> {
        matches!(self, Family::Gpt1 | Family::Gpt15).then_some("high")
    }
}

/// The ordered list of models to try for a request: the requested model
/// first, then the fallbacks, minus models already known to be unavailable.
/// If every model is known to be unavailable, the full list is returned so a
/// later call gets a fresh (zero-cost) walk instead of failing forever.
pub fn candidate_models(requested: &str, unavailable: &[String]) -> Vec<String> {
    let requested = requested.trim();
    let mut all: Vec<String> = Vec::new();
    if !requested.is_empty() {
        all.push(requested.to_string());
    }
    for m in FALLBACK_MODELS {
        if m != requested {
            all.push(m.to_string());
        }
    }
    let alive: Vec<String> = all.iter().filter(|m| !unavailable.contains(m)).cloned().collect();
    if alive.is_empty() {
        all
    } else {
        alive
    }
}

/// The quality string to send, clamped to what the model accepts.
pub fn api_quality(family: Family, q: Quality) -> &'static str {
    match q {
        Quality::XHigh | Quality::Max if !family.extended_quality() => Quality::High.as_str(),
        other => other.as_str(),
    }
}

/// Whether `w`x`h` satisfies the gpt-image-2 family's custom-size rules.
pub fn custom_size_ok(w: u32, h: u32) -> bool {
    if w == 0 || h == 0 || !w.is_multiple_of(SIZE_STEP) || !h.is_multiple_of(SIZE_STEP) {
        return false;
    }
    if w.max(h) > MAX_EDGE {
        return false;
    }
    let px = w as u64 * h as u64;
    if !(MIN_PIXELS..=MAX_PIXELS).contains(&px) {
        return false;
    }
    let aspect = w.max(h) as f64 / w.min(h) as f64;
    aspect <= MAX_ASPECT
}

/// The closest size to `w`x`h` (aspect first, then area) that satisfies
/// [`custom_size_ok`]. Returns the input unchanged when it is already valid.
pub fn snap_custom(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    if custom_size_ok(w, h) {
        return (w, h);
    }
    let aspect = (w as f64 / h as f64).clamp(1.0 / MAX_ASPECT, MAX_ASPECT);
    let mut area = (w as f64 * h as f64).clamp(MIN_PIXELS as f64, MAX_PIXELS as f64);
    // Respect the edge limit at this aspect.
    let edge_area_limit = if aspect >= 1.0 {
        MAX_EDGE as f64 * (MAX_EDGE as f64 / aspect)
    } else {
        MAX_EDGE as f64 * (MAX_EDGE as f64 * aspect)
    };
    area = area.min(edge_area_limit);
    let tw = (area * aspect).sqrt();
    let th = tw / aspect;

    // Multiples of 16 around the ideal edge; rounding can push the area or
    // aspect just past a limit, so one step either side is considered too.
    let around = |v: f64| {
        let lo = ((v / SIZE_STEP as f64).floor() as u32).max(1);
        [lo.saturating_sub(1).max(1), lo, lo + 1, lo + 2].map(|k| k * SIZE_STEP)
    };
    let mut best: Option<((u32, u32), f64)> = None;
    for cw in around(tw) {
        for ch in around(th) {
            if !custom_size_ok(cw, ch) {
                continue;
            }
            let a_err = ((cw as f64 / ch as f64).ln() - aspect.ln()).abs();
            let s_err = ((cw as f64 * ch as f64).ln() - area.ln()).abs();
            let score = a_err * 10.0 + s_err;
            if best.is_none_or(|(_, s)| score < s) {
                best = Some(((cw, ch), score));
            }
        }
    }
    best.map(|(s, _)| s).unwrap_or((1024, 1024))
}

/// The standard size whose aspect ratio is closest to `w`x`h`.
pub fn nearest_standard(w: u32, h: u32) -> (u32, u32) {
    let target = (w.max(1) as f64 / h.max(1) as f64).ln();
    STANDARD_SIZES
        .into_iter()
        .min_by(|a, b| {
            let da = ((a.0 as f64 / a.1 as f64).ln() - target).abs();
            let db = ((b.0 as f64 / b.1 as f64).ln() - target).abs();
            da.total_cmp(&db)
        })
        .expect("STANDARD_SIZES is not empty")
}

/// The size to ask the API for, given the model family and the requested
/// frame size. The result is resized back to the requested size afterward.
pub fn api_size(family: Family, w: u32, h: u32) -> (u32, u32) {
    if family.custom_sizes() {
        snap_custom(w, h)
    } else if STANDARD_SIZES.contains(&(w, h)) {
        (w, h)
    } else {
        nearest_standard(w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families() {
        assert_eq!(family("gpt-image-2.5-sunburst"), Family::Gpt25);
        assert_eq!(family("gpt-image-2.5-flare-2026-09-08"), Family::Gpt25);
        assert_eq!(family("gpt-image-2"), Family::Gpt2);
        assert_eq!(family("gpt-image-2-2026-04-21"), Family::Gpt2);
        assert_eq!(family("gpt-image-1.5"), Family::Gpt15);
        assert_eq!(family("gpt-image-1"), Family::Gpt1);
        assert_eq!(family("gpt-image-1-mini"), Family::Gpt1Mini);
        assert_eq!(family("chatgpt-image-latest"), Family::Unknown);
        assert_eq!(family("gpt-image-10"), Family::Unknown);
    }

    #[test]
    fn capabilities() {
        assert_eq!(Family::Gpt1.input_fidelity(), Some("high"));
        assert_eq!(Family::Gpt15.input_fidelity(), Some("high"));
        assert_eq!(Family::Gpt1Mini.input_fidelity(), None);
        assert_eq!(Family::Gpt2.input_fidelity(), None);
        assert_eq!(Family::Gpt25.input_fidelity(), None);
        assert_eq!(api_quality(Family::Gpt25, Quality::Max), "max");
        assert_eq!(api_quality(Family::Gpt25, Quality::XHigh), "xhigh");
        assert_eq!(api_quality(Family::Gpt2, Quality::XHigh), "high");
        assert_eq!(api_quality(Family::Gpt1, Quality::Max), "high");
        assert_eq!(api_quality(Family::Gpt1, Quality::Low), "low");
        assert_eq!(api_quality(Family::Gpt2, Quality::Auto), "auto");
    }

    #[test]
    fn candidates_start_with_requested_and_skip_dead() {
        let c = candidate_models("gpt-image-2", &[]);
        assert_eq!(
            c,
            ["gpt-image-2", "gpt-image-2.5-sunburst", "gpt-image-2.5-flare", "gpt-image-1.5", "gpt-image-1"]
        );
        let dead = vec!["gpt-image-2.5-sunburst".to_string()];
        let c = candidate_models("gpt-image-2.5-sunburst", &dead);
        assert_eq!(c[0], "gpt-image-2.5-flare");
        assert_eq!(c.len(), 4);
        let c = candidate_models("my-custom-model", &[]);
        assert_eq!(c.len(), 6);
        assert_eq!(c[0], "my-custom-model");
        let all_dead: Vec<String> = FALLBACK_MODELS.iter().map(|s| s.to_string()).collect();
        assert_eq!(candidate_models("gpt-image-1", &all_dead).len(), 5);
        assert_eq!(candidate_models("  ", &[])[0], "gpt-image-2.5-sunburst");
    }

    #[test]
    fn custom_size_rules() {
        assert!(custom_size_ok(1536, 1024));
        assert!(custom_size_ok(1024, 1024));
        assert!(custom_size_ok(3840, 2160));
        assert!(custom_size_ok(2160, 3840));
        assert!(custom_size_ok(2048, 1152));
        assert!(!custom_size_ok(1000, 1000), "not a multiple of 16");
        assert!(!custom_size_ok(512, 512), "too few pixels");
        assert!(!custom_size_ok(4096, 2048), "edge too long");
        assert!(!custom_size_ok(3840, 3840), "too many pixels");
        assert!(!custom_size_ok(2560, 768), "aspect over 3:1");
        assert!(!custom_size_ok(0, 1024));
    }

    #[test]
    fn snap_custom_produces_valid_nearby_sizes() {
        assert_eq!(snap_custom(1536, 1024), (1536, 1024));
        for (w, h) in [
            (1000, 1000),
            (512, 512),
            (64, 64),
            (4096, 2048),
            (8000, 8000),
            (5000, 1000),
            (1000, 5000),
            (1919, 1081),
            (1, 1),
        ] {
            let (sw, sh) = snap_custom(w, h);
            assert!(custom_size_ok(sw, sh), "{w}x{h} -> {sw}x{sh} invalid");
            let want = (w as f64 / h as f64).clamp(1.0 / 3.0, 3.0);
            let got = sw as f64 / sh as f64;
            assert!((got / want - 1.0).abs() < 0.05, "{w}x{h} -> {sw}x{sh} aspect drift");
        }
        let (sw, sh) = snap_custom(1919, 1081);
        assert!((sw as i64 - 1919).abs() <= 16 && (sh as i64 - 1081).abs() <= 16, "{sw}x{sh}");
    }

    #[test]
    fn standard_snapping_by_aspect() {
        assert_eq!(nearest_standard(1536, 1024), (1536, 1024));
        assert_eq!(nearest_standard(1920, 1080), (1536, 1024));
        assert_eq!(nearest_standard(1080, 1920), (1024, 1536));
        assert_eq!(nearest_standard(2048, 2048), (1024, 1024));
        assert_eq!(nearest_standard(1100, 1000), (1024, 1024));
        assert_eq!(api_size(Family::Gpt1, 1536, 1024), (1536, 1024));
        assert_eq!(api_size(Family::Gpt15, 1920, 1080), (1536, 1024));
        assert_eq!(api_size(Family::Gpt2, 1920, 1088), (1920, 1088));
        assert_eq!(api_size(Family::Gpt25, 2048, 2048), (2048, 2048));
    }
}
