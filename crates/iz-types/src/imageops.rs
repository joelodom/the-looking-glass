//! Small image operations shared by the pipeline, the mocks and the UI.
//! All functions work on `image::RgbaImage`.

use image::imageops::FilterType;
use image::{ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::Path;

/// A rectangle in normalized image coordinates (0..1, origin top-left).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Centered rectangle of the given normalized width and height.
    pub fn centered(w: f32, h: f32) -> Self {
        Self { x: (1.0 - w) / 2.0, y: (1.0 - h) / 2.0, w, h }
    }

    pub fn approx_eq(&self, o: &RectF, eps: f32) -> bool {
        (self.x - o.x).abs() < eps
            && (self.y - o.y).abs() < eps
            && (self.w - o.w).abs() < eps
            && (self.h - o.h).abs() < eps
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }

    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// Clamp to the unit square, keeping size where possible.
    pub fn clamped(&self) -> RectF {
        let w = self.w.clamp(0.0, 1.0);
        let h = self.h.clamp(0.0, 1.0);
        let x = self.x.clamp(0.0, 1.0 - w);
        let y = self.y.clamp(0.0, 1.0 - h);
        RectF { x, y, w, h }
    }

    /// Normalize a rect given by two corners in any order.
    pub fn from_corners(x0: f32, y0: f32, x1: f32, y1: f32) -> RectF {
        RectF { x: x0.min(x1), y: y0.min(y1), w: (x1 - x0).abs(), h: (y1 - y0).abs() }
    }
}

/// Adjust a drag rectangle so that, in pixels, it has the target aspect
/// ratio (width / height), anchored at `(ax, ay)` (the drag start, in
/// normalized coordinates) and extending toward the other corner. The larger
/// dimension is shrunk so the result never grows past the drag. The result is
/// clamped to the image.
pub fn snap_rect_to_aspect(
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    img_w: u32,
    img_h: u32,
    target_aspect: f32,
) -> RectF {
    let (iw, ih) = (img_w.max(1) as f32, img_h.max(1) as f32);
    let dx_px = (bx - ax) * iw;
    let dy_px = (by - ay) * ih;
    let (mut w_px, mut h_px) = (dx_px.abs(), dy_px.abs());
    if w_px < 1.0 && h_px < 1.0 {
        return RectF::new(ax, ay, 0.0, 0.0);
    }
    // Shrink whichever dimension is too large for the target aspect.
    if w_px / h_px.max(1e-6) > target_aspect {
        w_px = h_px * target_aspect;
    } else {
        h_px = w_px / target_aspect;
    }
    let w = w_px / iw;
    let h = h_px / ih;
    let x = if dx_px >= 0.0 { ax } else { ax - w };
    let y = if dy_px >= 0.0 { ay } else { ay - h };
    RectF { x, y, w, h }.clamped()
}

pub fn load_rgba(path: &Path) -> Result<RgbaImage, String> {
    image::open(path).map(|d| d.to_rgba8()).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn save_png(img: &RgbaImage, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = encode_png(img)?;
    crate::settings::write_atomic(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(buf)
}

pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    image::load_from_memory(bytes).map(|d| d.to_rgba8()).map_err(|e| e.to_string())
}

/// Crop a normalized rect out of an image (at least 1x1).
pub fn crop_norm(img: &RgbaImage, rect: RectF) -> RgbaImage {
    let r = rect.clamped();
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let x = (r.x * iw).round().clamp(0.0, iw - 1.0) as u32;
    let y = (r.y * ih).round().clamp(0.0, ih - 1.0) as u32;
    let w = ((r.w * iw).round() as u32).clamp(1, img.width() - x);
    let h = ((r.h * ih).round() as u32).clamp(1, img.height() - y);
    image::imageops::crop_imm(img, x, y, w, h).to_image()
}

pub fn resize_to(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    image::imageops::resize(img, w.max(1), h.max(1), FilterType::Lanczos3)
}

/// Scale to fit inside `max_w` x `max_h`, preserving aspect.
pub fn fit_within(img: &RgbaImage, max_w: u32, max_h: u32) -> RgbaImage {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let s = (max_w as f32 / w).min(max_h as f32 / h).min(1.0);
    resize_to(img, (w * s).round() as u32, (h * s).round() as u32)
}

pub fn thumbnail(img: &RgbaImage, max_w: u32) -> RgbaImage {
    fit_within(img, max_w, max_w)
}

/// Place `img`, scaled so its width is `scale` of the canvas width, in the
/// middle of a fully transparent canvas. Returns `(canvas, mask)` where the
/// mask is opaque over the placed image and transparent elsewhere, which is
/// what the image edit endpoint expects (transparent = paint here). Also
/// returns the rect the image occupies, normalized to the canvas.
pub fn center_on_transparent(
    img: &RgbaImage,
    canvas_w: u32,
    canvas_h: u32,
    scale: f32,
) -> (RgbaImage, RgbaImage, RectF) {
    let target_w = ((canvas_w as f32) * scale).round().max(1.0) as u32;
    let target_h = ((target_w as f32) * (img.height() as f32 / img.width().max(1) as f32)).round().max(1.0) as u32;
    let target_h = target_h.min(canvas_h);
    let placed = resize_to(img, target_w, target_h);
    let ox = (canvas_w.saturating_sub(target_w)) / 2;
    let oy = (canvas_h.saturating_sub(target_h)) / 2;
    let mut canvas = RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 0]));
    let mut mask = RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 0]));
    for (x, y, p) in placed.enumerate_pixels() {
        let (cx, cy) = (x + ox, y + oy);
        if cx < canvas_w && cy < canvas_h {
            canvas.put_pixel(cx, cy, Rgba([p[0], p[1], p[2], 255]));
            mask.put_pixel(cx, cy, Rgba([0, 0, 0, 255]));
        }
    }
    let rect = RectF::new(
        ox as f32 / canvas_w as f32,
        oy as f32 / canvas_h as f32,
        target_w as f32 / canvas_w as f32,
        target_h as f32 / canvas_h as f32,
    );
    (canvas, mask, rect)
}

/// Force every pixel opaque (the image API returns RGBA for PNG; references
/// should be opaque).
pub fn opaque(img: &RgbaImage) -> RgbaImage {
    let mut out = img.clone();
    for p in out.pixels_mut() {
        p[3] = 255;
    }
    out
}

/// Draw a 1-pixel-wide (scaled by `thickness`) rectangle outline. Used by the
/// mocks to show the source rect.
pub fn draw_rect_outline(img: &mut RgbaImage, rect: RectF, color: Rgba<u8>, thickness: u32) {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let r = rect.clamped();
    let x0 = (r.x * iw) as u32;
    let y0 = (r.y * ih) as u32;
    let x1 = ((r.x + r.w) * iw).min(iw - 1.0) as u32;
    let y1 = ((r.y + r.h) * ih).min(ih - 1.0) as u32;
    for t in 0..thickness {
        for x in x0..=x1 {
            if y0 + t < img.height() {
                img.put_pixel(x, y0 + t, color);
            }
            if y1 >= t {
                img.put_pixel(x, y1 - t, color);
            }
        }
        for y in y0..=y1 {
            if x0 + t < img.width() {
                img.put_pixel(x0 + t, y, color);
            }
            if x1 >= t {
                img.put_pixel(x1 - t, y, color);
            }
        }
    }
}

/// Blend a flat color over the image. `alpha` 0..1.
pub fn tint(img: &mut RgbaImage, color: [u8; 3], alpha: f32) {
    let a = alpha.clamp(0.0, 1.0);
    for p in img.pixels_mut() {
        for i in 0..3 {
            p[i] = ((p[i] as f32) * (1.0 - a) + (color[i] as f32) * a).round() as u8;
        }
        p[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_keeps_aspect_and_anchor() {
        let r = snap_rect_to_aspect(0.1, 0.1, 0.5, 0.9, 1536, 1024, 1.5);
        let w_px = r.w * 1536.0;
        let h_px = r.h * 1024.0;
        assert!((w_px / h_px - 1.5).abs() < 0.01, "aspect {}", w_px / h_px);
        assert!((r.x - 0.1).abs() < 1e-5 && (r.y - 0.1).abs() < 1e-5);
        assert!(r.w <= 0.4 + 1e-5 && r.h <= 0.8 + 1e-5);
    }

    #[test]
    fn snap_handles_reverse_drag() {
        let r = snap_rect_to_aspect(0.9, 0.9, 0.5, 0.1, 1536, 1024, 1.5);
        assert!((r.x + r.w - 0.9).abs() < 1e-4);
        assert!((r.y + r.h - 0.9).abs() < 1e-4);
    }

    #[test]
    fn center_on_transparent_builds_mask() {
        let img = RgbaImage::from_pixel(300, 200, Rgba([10, 20, 30, 255]));
        let (canvas, mask, rect) = center_on_transparent(&img, 1536, 1024, 0.25);
        assert_eq!(canvas.dimensions(), (1536, 1024));
        assert_eq!(mask.dimensions(), (1536, 1024));
        assert_eq!(canvas.get_pixel(0, 0)[3], 0);
        assert_eq!(mask.get_pixel(0, 0)[3], 0);
        let (cx, cy) = (768, 512);
        assert_eq!(canvas.get_pixel(cx, cy)[3], 255);
        assert_eq!(mask.get_pixel(cx, cy)[3], 255);
        assert!((rect.w - 0.25).abs() < 0.01);
    }

    #[test]
    fn crop_norm_rounds_and_clamps() {
        let img = RgbaImage::from_pixel(100, 50, Rgba([1, 2, 3, 255]));
        let c = crop_norm(&img, RectF::new(0.5, 0.5, 0.6, 0.6));
        // clamped() keeps the size and shifts the origin: x 0.4..1.0, y 0.4..1.0
        assert_eq!(c.dimensions(), (60, 30));
    }
}
