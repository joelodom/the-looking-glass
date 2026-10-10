//! GPU textures for the current frame and the saved-world thumbnails.
//!
//! The frame texture is cached by (world, node, image version, path), so a
//! regenerate (which bumps the version) reloads it and anything else reuses
//! it. Only one frame texture is kept; replacing it drops the old one, which
//! frees it on the GPU.

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use iz_types::{NodeId, WorldId, WorldView};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Thumbnails are downscaled to at most this many pixels on the long side.
const THUMB_MAX_SIDE: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameKey {
    world: WorldId,
    node: NodeId,
    version: u32,
    path: PathBuf,
}

impl FrameKey {
    fn of(view: &WorldView) -> Self {
        Self {
            world: view.world_id.clone(),
            node: view.current,
            version: view.image_version,
            path: view.image_path.clone(),
        }
    }
}

/// The current frame on the GPU, with the size of the source image in pixels
/// (the texture itself may be smaller if the image exceeded the GPU limit).
pub struct FrameTexture {
    pub handle: TextureHandle,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub struct TextureCache {
    frame: Option<(FrameKey, Result<FrameTexture, String>)>,
    /// None = tried and failed (missing or unreadable file); not retried
    /// until the cache is cleared.
    thumbs: HashMap<PathBuf, Option<TextureHandle>>,
}

impl TextureCache {
    /// The texture for the view's current frame, loading it on first use.
    /// A load failure is remembered for that key and returned as text.
    pub fn frame(&mut self, ctx: &Context, view: &WorldView) -> Result<&FrameTexture, &str> {
        let key = FrameKey::of(view);
        if self.frame.as_ref().map(|(k, _)| k != &key).unwrap_or(true) {
            let max_side = ctx.input(|i| i.max_texture_side) as u32;
            let loaded = load_color_image(&view.image_path, max_side).map(|(img, width, height)| {
                let name = format!("frame-{}-{}-v{}", view.world_id, view.current, view.image_version);
                FrameTexture {
                    handle: ctx.load_texture(name, img, TextureOptions::LINEAR),
                    width,
                    height,
                }
            });
            self.frame = Some((key, loaded));
        }
        match &self.frame {
            Some((_, Ok(t))) => Ok(t),
            Some((_, Err(e))) => Err(e.as_str()),
            None => Err("no frame"),
        }
    }

    /// A small texture for a saved world's thumbnail, or None if it cannot
    /// be read.
    pub fn thumbnail(&mut self, ctx: &Context, path: &Path) -> Option<&TextureHandle> {
        if !self.thumbs.contains_key(path) {
            let max_side = (ctx.input(|i| i.max_texture_side) as u32).min(THUMB_MAX_SIDE);
            let handle = load_color_image(path, max_side)
                .ok()
                .map(|(img, _, _)| ctx.load_texture(format!("thumb-{}", path.display()), img, TextureOptions::LINEAR));
            self.thumbs.insert(path.to_path_buf(), handle);
        }
        self.thumbs.get(path).and_then(|t| t.as_ref())
    }

    /// Drop a remembered frame load failure so the next draw retries (the
    /// engine may announce a view a moment before its PNG is on disk).
    pub fn retry_failed_frame(&mut self) {
        if matches!(self.frame, Some((_, Err(_)))) {
            self.frame = None;
        }
    }

    /// Forget thumbnails (they are rewritten as worlds change).
    pub fn clear_thumbnails(&mut self) {
        self.thumbs.clear();
    }
}

/// Decode an image file into an egui image no larger than `max_side` on
/// either side. Returns the original pixel size too.
fn load_color_image(path: &Path, max_side: u32) -> Result<(ColorImage, u32, u32), String> {
    let img = iz_types::imageops::load_rgba(path)?;
    let (width, height) = img.dimensions();
    let img = if width > max_side || height > max_side {
        iz_types::imageops::fit_within(&img, max_side, max_side)
    } else {
        img
    };
    let size = [img.width() as usize, img.height() as usize];
    Ok((ColorImage::from_rgba_unmultiplied(size, img.as_raw()), width, height))
}
