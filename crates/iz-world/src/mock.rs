//! Mock Describer and Painter: no network, no cost.
//!
//! They sleep like the real services (cancellable), log Request/Response
//! events like the real services, and return plausible specs and visibly
//! distinct, always opaque, frame-sized images.

use crate::pipeline::{transition_noun, RealEngine};
use image::{Rgba, RgbaImage};
use iz_types::imageops::{self, RectF};
use iz_types::log::{CallLog, LogEvent, LogKind};
use iz_types::{
    CallContext, Camera, CanvasMode, DescribeError, DescribeKind, DescribeRequest, Describer, PaintError, PaintRequest,
    PaintResult, Painter, SceneSpec, Service, Transition,
};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SERVICE: &str = "mock";

/// An engine over [`MockDescriber`] and [`MockPainter`] with their default
/// delays (about 1 s and 2 s). Status events name `Service::Mock`.
pub fn mock_engine(log: Arc<dyn CallLog>) -> RealEngine {
    RealEngine::with_services(
        Arc::new(MockDescriber::new()),
        Arc::new(MockPainter::new()),
        log,
        Service::Mock,
        Service::Mock,
    )
}

/// Sleep for `total`, waking at least every 100 ms to check the cancel flag.
/// Returns false when cancelled.
fn sleep_cancellable(total: Duration, ctx: &CallContext) -> bool {
    let end = Instant::now() + total;
    loop {
        if ctx.cancelled() {
            return false;
        }
        let now = Instant::now();
        if now >= end {
            return true;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Deterministic pseudo-random sequence from a seed.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn time_seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or_default()
}

/// Regime guessed from the frame width, using the vocabulary in `Camera`.
pub fn regime_for(frame_width_m: f64) -> &'static str {
    match frame_width_m {
        w if w > 1e9 => "cosmic",
        w if w > 1e6 => "planetary",
        w if w > 1e3 => "geographic",
        w if w > 10.0 => "street",
        w if w > 0.3 => "human",
        w if w > 1e-3 => "eye",
        w if w > 1e-5 => "cellular",
        w if w > 1e-8 => "molecular",
        _ => "imaginary",
    }
}

/// "about 40 km across", "2.0 mm across"...
pub fn scale_label(frame_width_m: f64) -> String {
    const LIGHT_YEAR: f64 = 9.460_730_472_580_8e15;
    const AU: f64 = 1.495_978_707e11;
    let m = frame_width_m.abs();
    let (value, unit) = match m {
        m if m >= LIGHT_YEAR => (m / LIGHT_YEAR, "light years"),
        m if m >= AU => (m / AU, "AU"),
        m if m >= 1e3 => (m / 1e3, "km"),
        m if m >= 1.0 => (m, "m"),
        m if m >= 1e-2 => (m * 1e2, "cm"),
        m if m >= 1e-3 => (m * 1e3, "mm"),
        m if m >= 1e-6 => (m * 1e6, "µm"),
        m if m >= 1e-9 => (m * 1e9, "nm"),
        m => (m * 1e12, "pm"),
    };
    let value = match value {
        v if v >= 10.0 => format!("{v:.0}"),
        v if v >= 0.1 => format!("{v:.1}"),
        v => format!("{v:.1e}"),
    };
    format!("about {value} {unit} across")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn path_text(p: Option<&Path>) -> String {
    p.map(|p| p.display().to_string()).unwrap_or_else(|| "none".into())
}

fn append_fact(dossier: &mut Value, fact: String) {
    if !dossier.is_object() {
        *dossier = json!({});
    }
    if let Some(obj) = dossier.as_object_mut() {
        match obj.get_mut("facts") {
            Some(Value::Array(facts)) => facts.push(Value::String(fact)),
            _ => {
                obj.insert("facts".into(), json!([fact]));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Describer

/// Fake Claude: returns a plausible [`SceneSpec`] after a short sleep.
pub struct MockDescriber {
    delay: Duration,
}

impl MockDescriber {
    /// Sleeps about 1 s per call.
    pub fn new() -> Self {
        Self::with_delay(Duration::from_secs(1))
    }

    pub fn with_delay(delay: Duration) -> Self {
        Self { delay }
    }
}

impl Default for MockDescriber {
    fn default() -> Self {
        Self::new()
    }
}

/// The purpose string the pipeline uses for this request.
fn describe_purpose(req: &DescribeRequest) -> String {
    let noun = match (&req.kind, &req.transition) {
        (DescribeKind::Lucky, _) => "lucky",
        (_, Transition::Root) if req.current_image.is_none() => "lucky",
        (_, t) => transition_noun(t),
    };
    match req.kind {
        DescribeKind::Rewrite { .. } => format!("rewrite {noun}"),
        _ => format!("describe {noun}"),
    }
}

fn describe_request_text(req: &DescribeRequest) -> String {
    let kind = match &req.kind {
        DescribeKind::Root => "root".to_string(),
        DescribeKind::Lucky => "lucky".to_string(),
        DescribeKind::Step => "step".to_string(),
        DescribeKind::Rewrite { refused_message, .. } => format!("rewrite (refused: {refused_message})"),
    };
    let ancestry: Vec<&str> = req.ancestry.iter().map(|a| a.title.as_str()).collect();
    format!(
        "kind: {kind}\ntransition: {}\ncurrent image: {}\ncrop image: {}\nparent image: {}\n\
         prior spec title: {}\nproposed camera: {}\nancestry: {}\nworld dir: {}",
        to_json(&req.transition),
        path_text(req.current_image.as_deref()),
        path_text(req.crop_image.as_deref()),
        path_text(req.parent_image.as_deref()),
        req.prior_spec.as_ref().map(|s| s.title.as_str()).unwrap_or("none"),
        req.proposed_camera.as_ref().map(to_json).unwrap_or_else(|| "none".into()),
        if ancestry.is_empty() { "none".to_string() } else { ancestry.join(" > ") },
        req.world_dir.display(),
    )
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn root_spec(req: &DescribeRequest) -> SceneSpec {
    let frame_width_m = 30.0;
    let camera = Camera {
        frame_width_m,
        distance_m: 25.0,
        tilt_deg: 60.0,
        heading_deg: 0.0,
        regime: regime_for(frame_width_m).into(),
        scale_label: scale_label(frame_width_m),
    };
    let source = req
        .current_image
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the photo".into());
    SceneSpec {
        title: "A street in an unidentified town".into(),
        render_prompt: "A realistic photograph of a quiet street in a small town, afternoon light.".into(),
        camera,
        canvas_mode: CanvasMode::Reference,
        dossier: json!({
            "location": "an unidentified small town",
            "time_of_day": "afternoon",
            "season": "summer",
            "weather": "clear",
            "facts": [format!("the starting photo ({source}) shows a street scene")],
        }),
        notes: "mock describer: guessed a street-level photo".into(),
    }
}

const LUCKY_PLACES: &[&str] = &[
    "Fushimi Inari Taisha, Kyoto, Japan",
    "Hallstatt, Austria",
    "Valparaíso, Chile",
    "Chefchaouen, Morocco",
    "Reykjavík harbour, Iceland",
    "Göreme, Cappadocia, Turkey",
    "Old Town Square, Prague, Czechia",
    "Lake Bled, Slovenia",
];

fn lucky_spec() -> SceneSpec {
    let mut seed = time_seed();
    let place = LUCKY_PLACES[(splitmix(&mut seed) % LUCKY_PLACES.len() as u64) as usize];
    let heading_deg = (splitmix(&mut seed) % 8) as f32 * 45.0;
    let frame_width_m = 1500.0;
    let camera = Camera {
        frame_width_m,
        distance_m: 1200.0,
        tilt_deg: 45.0,
        heading_deg,
        regime: regime_for(frame_width_m).into(),
        scale_label: scale_label(frame_width_m),
    };
    SceneSpec {
        title: format!("{place} from the air"),
        render_prompt: format!(
            "An aerial photograph of {place}, looking toward heading {heading_deg:.0}°, late afternoon light, \
             realistic, high detail."
        ),
        camera,
        canvas_mode: CanvasMode::Reference,
        dossier: json!({
            "location": place,
            "time_of_day": "late afternoon",
            "season": "autumn",
            "facts": [format!("lucky pick: {place}")],
        }),
        notes: "mock describer: picked a place from a short list".into(),
    }
}

fn step_spec(req: &DescribeRequest) -> SceneSpec {
    let prior = req.prior_spec.as_ref();
    let mut camera = req.proposed_camera.clone().or_else(|| prior.map(|p| p.camera.clone())).unwrap_or_default();
    let old_regime = prior.map(|p| p.camera.regime.clone()).unwrap_or_default();
    camera.regime = regime_for(camera.frame_width_m).into();
    camera.scale_label = scale_label(camera.frame_width_m);
    let label = req.transition.label();
    let title = format!("{} ({}, {})", capitalize(&label), camera.regime, camera.scale_label);
    let canvas_mode = match req.transition {
        Transition::ZoomIn { rect } if rect.w > 0.33 => CanvasMode::Crop,
        _ => CanvasMode::Reference,
    };
    let mut dossier = prior.map(|p| p.dossier.clone()).unwrap_or(Value::Null);
    append_fact(&mut dossier, format!("depth {}: {label} reached {}", req.ancestry.len() + 1, camera.scale_label));
    let notes = if !old_regime.is_empty() && old_regime != camera.regime {
        format!("mock describer: regime change {old_regime} -> {}", camera.regime)
    } else {
        "mock describer".into()
    };
    SceneSpec {
        render_prompt: format!(
            "Mock render: the {} scene, {}, tilt {:.0}°, heading {:.0}°, after a {label}. Keep continuity with \
             the reference frames.",
            camera.regime, camera.scale_label, camera.tilt_deg, camera.heading_deg
        ),
        title,
        camera,
        canvas_mode,
        dossier,
        notes,
    }
}

fn rewrite_spec(previous: &SceneSpec, refused_message: &str) -> SceneSpec {
    SceneSpec {
        render_prompt: format!(
            "A generic, safe, family-friendly rendering; everyone fully clothed. {}",
            previous.render_prompt
        ),
        notes: format!("mock describer: rewritten after a refusal ({refused_message})"),
        ..previous.clone()
    }
}

impl Describer for MockDescriber {
    fn describe(&self, req: &DescribeRequest, ctx: &CallContext) -> Result<SceneSpec, DescribeError> {
        let purpose = describe_purpose(req);
        ctx.log(LogEvent::new(SERVICE, &purpose, LogKind::Request, describe_request_text(req)));
        if !sleep_cancellable(self.delay, ctx) {
            ctx.log(LogEvent::new(SERVICE, &purpose, LogKind::Note, "cancelled"));
            return Err(DescribeError::Cancelled);
        }
        let spec = match &req.kind {
            DescribeKind::Root => root_spec(req),
            DescribeKind::Lucky => lucky_spec(),
            DescribeKind::Step => step_spec(req),
            DescribeKind::Rewrite { refused_message, previous } => rewrite_spec(previous, refused_message),
        };
        let reply = serde_json::to_string_pretty(&spec).unwrap_or_default();
        ctx.log(LogEvent::new(SERVICE, &purpose, LogKind::Response, reply));
        Ok(spec)
    }
}

// ---------------------------------------------------------------------------
// Painter

/// Fake image model: returns a visibly distinct frame-size image per call.
pub struct MockPainter {
    delay: Duration,
    refusals: AtomicUsize,
}

impl MockPainter {
    /// Sleeps about 2 s per call.
    pub fn new() -> Self {
        Self::with_delay(Duration::from_secs(2))
    }

    pub fn with_delay(delay: Duration) -> Self {
        Self { delay, refusals: AtomicUsize::new(0) }
    }

    /// Refuse the next `n` calls as a moderation refusal would, to exercise
    /// the rewrite path.
    pub fn refuse_next(&self, n: usize) {
        self.refusals.store(n, Ordering::SeqCst);
    }
}

impl Default for MockPainter {
    fn default() -> Self {
        Self::new()
    }
}

fn dims(img: &RgbaImage) -> String {
    format!("{}x{}", img.width(), img.height())
}

fn paint_request_text(req: &PaintRequest) -> String {
    let references: Vec<String> = req.references.iter().map(dims).collect();
    format!(
        "model: {}\nquality: {}\nsize: {}x{}\ncanvas: {}\nmask: {}\nreferences: {}\nprompt:\n{}",
        req.model,
        req.quality.as_str(),
        req.width,
        req.height,
        req.canvas.as_ref().map(|c| format!("{} (in memory)", dims(c))).unwrap_or_else(|| "none".into()),
        req.mask.as_ref().map(|m| format!("{} (in memory)", dims(m))).unwrap_or_else(|| "none".into()),
        if references.is_empty() { "none".to_string() } else { references.join(", ") },
        req.prompt,
    )
}

fn hash_color(h: u64) -> [u8; 3] {
    // Keep colors away from the extremes so tints stay visible.
    [(h & 0xff) as u8 / 2 + 64, ((h >> 8) & 0xff) as u8 / 2 + 64, ((h >> 16) & 0xff) as u8 / 2 + 64]
}

fn sized(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    if img.dimensions() == (w, h) {
        img.clone()
    } else {
        imageops::resize_to(img, w, h)
    }
}

fn gradient(w: u32, h: u32, top: [u8; 3], bottom: [u8; 3]) -> RgbaImage {
    let span = h.saturating_sub(1).max(1) as f32;
    RgbaImage::from_fn(w, h, |_, y| {
        let t = y as f32 / span;
        let mix = |i: usize| (f32::from(top[i]) * (1.0 - t) + f32::from(bottom[i]) * t).round() as u8;
        Rgba([mix(0), mix(1), mix(2), 255])
    })
}

/// `top` composited over `bottom` by `top`'s alpha.
fn composite(top: &RgbaImage, bottom: &RgbaImage) -> RgbaImage {
    RgbaImage::from_fn(bottom.width(), bottom.height(), |x, y| {
        let b = bottom.get_pixel(x, y);
        let t = top.get_pixel(x, y);
        let a = f32::from(t[3]) / 255.0;
        let mix = |i: usize| (f32::from(t[i]) * a + f32::from(b[i]) * (1.0 - a)).round() as u8;
        Rgba([mix(0), mix(1), mix(2), 255])
    })
}

/// Normalized bounding box of the non-transparent pixels.
fn opaque_bbox(img: &RgbaImage) -> Option<RectF> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 <= x1).then(|| {
        let (w, h) = (img.width() as f32, img.height() as f32);
        RectF::new(x0 as f32 / w, y0 as f32 / h, (x1 - x0 + 1) as f32 / w, (y1 - y0 + 1) as f32 / h)
    })
}

fn shifted(img: &RgbaImage, dx: u32, dy: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    RgbaImage::from_fn(w, h, |x, y| *img.get_pixel((x + dx) % w, (y + dy) % h))
}

fn draw_shapes(img: &mut RgbaImage, seed: u64) {
    let (w, h) = img.dimensions();
    let mut s = seed;
    for i in 0..6 {
        let color = hash_color(splitmix(&mut s));
        let cx = (splitmix(&mut s) % u64::from(w)) as i64;
        let cy = (splitmix(&mut s) % u64::from(h)) as i64;
        let r = (splitmix(&mut s) % u64::from(w.min(h) / 6).max(1)) as i64 + i64::from(w.min(h) / 20) + 1;
        let pixel = Rgba([color[0], color[1], color[2], 255]);
        for y in (cy - r).max(0)..(cy + r).min(i64::from(h)) {
            for x in (cx - r).max(0)..(cx + r).min(i64::from(w)) {
                let inside = if i % 2 == 0 { (x - cx).pow(2) + (y - cy).pow(2) <= r * r } else { true };
                if inside {
                    img.put_pixel(x as u32, y as u32, pixel);
                }
            }
        }
    }
}

/// The mock image for a request, by its shape:
/// - canvas + mask (zoom out): the canvas over a two-color gradient;
/// - canvas only (zoom in, crop mode): the canvas tinted;
/// - references, zoom in (reference mode): the first reference tinted;
/// - references otherwise (look, forward): the first reference tinted and shifted;
/// - nothing (lucky): a gradient with a few shapes.
fn render(req: &PaintRequest) -> RgbaImage {
    let (w, h) = (req.width.max(1), req.height.max(1));
    let seed = fnv1a(&format!("{}\n{}", req.purpose, req.prompt));
    let color = hash_color(seed);
    let mut outline = RectF::new(0.02, 0.02, 0.96, 0.96);
    let mut img = match (&req.canvas, &req.mask, req.references.first()) {
        (Some(canvas), Some(_), _) => {
            let canvas = sized(canvas, w, h);
            if let Some(bbox) = opaque_bbox(&canvas) {
                outline = bbox;
            }
            composite(&canvas, &gradient(w, h, hash_color(seed >> 24), hash_color(seed >> 40)))
        }
        (Some(canvas), None, _) => {
            let mut img = sized(canvas, w, h);
            imageops::tint(&mut img, color, 0.25);
            img
        }
        (None, _, Some(first)) if req.purpose.contains("zoom in") => {
            let mut img = sized(first, w, h);
            imageops::tint(&mut img, color, 0.35);
            img
        }
        (None, _, Some(first)) => {
            let dx = (seed % u64::from((w / 4).max(1))) as u32 + w / 8;
            let dy = ((seed >> 32) % u64::from((h / 8).max(1))) as u32;
            let mut img = shifted(&sized(first, w, h), dx, dy);
            imageops::tint(&mut img, color, 0.3);
            outline = RectF::centered(0.5, 0.5);
            img
        }
        (None, _, None) => {
            let mut img = gradient(w, h, hash_color(seed >> 8), hash_color(seed >> 32));
            draw_shapes(&mut img, seed);
            img
        }
    };
    let ink = Rgba([255 - color[0], 255 - color[1], 255 - color[2], 255]);
    imageops::draw_rect_outline(&mut img, outline, ink, (w.min(h) / 128).max(2));
    imageops::opaque(&img)
}

impl Painter for MockPainter {
    fn paint(&self, req: &PaintRequest, ctx: &CallContext) -> Result<PaintResult, PaintError> {
        ctx.log(LogEvent::new(SERVICE, &req.purpose, LogKind::Request, paint_request_text(req)));
        if !sleep_cancellable(self.delay, ctx) {
            ctx.log(LogEvent::new(SERVICE, &req.purpose, LogKind::Note, "cancelled"));
            return Err(PaintError::Cancelled);
        }
        if self.refusals.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            let msg = "mock moderation refusal";
            ctx.log(LogEvent::new(SERVICE, &req.purpose, LogKind::Error, msg));
            return Err(PaintError::Refused(msg.into()));
        }
        let image = render(req);
        ctx.log(LogEvent::new(
            SERVICE,
            &req.purpose,
            LogKind::Response,
            format!("image {} (mock painter, no cost)", dims(&image)),
        ));
        Ok(PaintResult {
            image,
            model_used: "mock-painter".into(),
            usage: Some(json!({ "mock": true })),
            estimated_usd: Some(0.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::NullLog;
    use iz_types::Quality;
    use std::sync::atomic::AtomicBool;

    fn ctx() -> CallContext {
        CallContext::new(Arc::new(NullLog))
    }

    fn request(
        canvas: Option<RgbaImage>,
        mask: Option<RgbaImage>,
        references: Vec<RgbaImage>,
        purpose: &str,
    ) -> PaintRequest {
        PaintRequest {
            model: "m".into(),
            prompt: "a prompt".into(),
            width: 96,
            height: 64,
            quality: Quality::Low,
            canvas,
            mask,
            references,
            purpose: purpose.into(),
        }
    }

    #[test]
    fn regimes_and_labels() {
        assert_eq!(regime_for(2e9), "cosmic");
        assert_eq!(regime_for(40_000.0), "geographic");
        assert_eq!(regime_for(30.0), "street");
        assert_eq!(regime_for(1.0), "human");
        assert_eq!(regime_for(1e-4), "cellular");
        assert_eq!(regime_for(1e-9), "imaginary");
        assert_eq!(scale_label(40_000.0), "about 40 km across");
        assert_eq!(scale_label(0.002), "about 2.0 mm across");
    }

    #[test]
    fn every_paint_mode_is_frame_sized_and_opaque() {
        let painter = MockPainter::with_delay(Duration::ZERO);
        let photo = RgbaImage::from_pixel(30, 20, Rgba([200, 10, 10, 255]));
        let (canvas, mask, _) = imageops::center_on_transparent(&photo, 96, 64, 0.25);
        let cases = [
            request(Some(canvas), Some(mask), vec![photo.clone()], "paint zoom out"),
            request(Some(photo.clone()), None, vec![photo.clone()], "paint zoom in"),
            request(None, None, vec![photo.clone(), photo.clone()], "paint zoom in"),
            request(None, None, vec![photo.clone()], "paint look"),
            request(None, None, Vec::new(), "paint lucky"),
        ];
        for req in &cases {
            let out = painter.paint(req, &ctx()).unwrap().image;
            assert_eq!(out.dimensions(), (96, 64), "{}", req.purpose);
            assert!(out.pixels().all(|p| p[3] == 255), "{}", req.purpose);
        }
    }

    #[test]
    fn refusals_and_cancel() {
        let painter = MockPainter::with_delay(Duration::ZERO);
        painter.refuse_next(1);
        let req = request(None, None, Vec::new(), "paint lucky");
        assert!(matches!(painter.paint(&req, &ctx()), Err(PaintError::Refused(_))));
        assert!(painter.paint(&req, &ctx()).is_ok());

        let slow = MockPainter::with_delay(Duration::from_secs(5));
        let cancelled = CallContext { cancel: Arc::new(AtomicBool::new(true)), ..ctx() };
        let started = Instant::now();
        assert!(matches!(slow.paint(&req, &cancelled), Err(PaintError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn step_spec_follows_the_proposed_camera() {
        let proposed = Camera { frame_width_m: 0.5, ..Camera::default() };
        let req = DescribeRequest {
            kind: DescribeKind::Step,
            transition: Transition::ZoomIn { rect: RectF::new(0.0, 0.0, 0.5, 0.5) },
            current_image: None,
            crop_image: None,
            parent_image: None,
            prior_spec: Some(SceneSpec {
                title: "prior".into(),
                render_prompt: String::new(),
                camera: Camera::default(),
                canvas_mode: CanvasMode::Reference,
                dossier: json!({ "location": "here", "facts": ["one"] }),
                notes: String::new(),
            }),
            proposed_camera: Some(proposed),
            ancestry: Vec::new(),
            world_dir: std::env::temp_dir(),
            settings: Default::default(),
        };
        let spec = MockDescriber::with_delay(Duration::ZERO).describe(&req, &ctx()).unwrap();
        assert_eq!(spec.camera.frame_width_m, 0.5);
        assert_eq!(spec.camera.regime, "human");
        assert_eq!(spec.canvas_mode, CanvasMode::Crop);
        assert_eq!(spec.dossier["location"], "here");
        assert_eq!(spec.dossier["facts"].as_array().unwrap().len(), 2);
    }
}
