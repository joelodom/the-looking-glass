//! The open screen: open a photo (dialog or drop), "I'm feeling lucky", or
//! reopen a saved world.

use crate::app::{Intent, Message};
use crate::textures::TextureCache;
use egui::{Align2, Color32, FontId, Key, Modifiers, RichText, Sense, Ui, UiBuilder, Vec2};
use iz_types::{Action, BusyInfo, WorldSummary};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// What the image crate in this build can decode.
pub const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

const THUMB_SIZE: Vec2 = Vec2::new(150.0, 100.0);
const COLUMN_WIDTH: f32 = 720.0;

/// Keys on the open screen: only X (cancel) matters; Escape is the app's.
pub fn map_key(key: Key, mods: Modifiers) -> Option<Intent> {
    if mods.command || mods.ctrl || mods.alt || mods.mac_cmd {
        return None;
    }
    (key == Key::X).then_some(Intent::Cancel)
}

pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// "Oct 9, 2026 20:41" in local time, or the input unchanged if it is not
/// RFC 3339.
pub fn format_updated(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|d| d.with_timezone(&chrono::Local).format("%b %-d, %Y %H:%M").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}

pub fn show(
    ui: &mut Ui,
    worlds: &[WorldSummary],
    busy: Option<&BusyInfo>,
    messages: &VecDeque<Message>,
    textures: &mut TextureCache,
) -> Vec<Intent> {
    let mut intents = Vec::new();
    let is_busy = busy.is_some();
    intents.extend(dropped_photo(ui, is_busy));

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.set_max_width(COLUMN_WIDTH);
            ui.add_space(48.0);
            ui.label(RichText::new(iz_types::APP_NAME).size(40.0).strong());
            ui.add_space(4.0);
            ui.weak("Drag a rectangle to zoom in. Past the real pixels, AI paints what is there, without end.");
            ui.add_space(24.0);

            ui.horizontal(|ui| {
                // Center the two buttons in the column.
                let button_w = 180.0;
                let spacing = ui.spacing().item_spacing.x;
                ui.add_space(((ui.available_width() - 2.0 * button_w - spacing) / 2.0).max(0.0));
                let size = egui::vec2(button_w, 36.0);
                let open = ui.add_enabled(
                    !is_busy,
                    egui::Button::new(RichText::new("Open photo…").size(16.0)).min_size(size),
                );
                if open.clicked() {
                    if let Some(path) = pick_photo() {
                        intents.push(Intent::Submit(Action::OpenPhoto(path)));
                    }
                }
                let lucky = ui.add_enabled(
                    !is_busy,
                    egui::Button::new(RichText::new("I'm feeling lucky").size(16.0)).min_size(size),
                );
                if lucky.clicked() {
                    intents.push(Intent::Submit(Action::Lucky));
                }
            });
            ui.add_space(8.0);
            ui.weak("…or drop a photo anywhere on this window.");

            if let Some(m) = messages.back().filter(|m| m.is_error) {
                ui.add_space(12.0);
                ui.colored_label(ui.visuals().warn_fg_color, &m.text);
            }
            if is_busy {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Working… (X or the Cancel button below stops it)");
                });
            }

            ui.add_space(32.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new("Saved worlds").size(18.0).strong());
            ui.add_space(8.0);
            if worlds.is_empty() {
                ui.weak("No saved worlds yet.");
            }
            for world in worlds {
                if world_row(ui, world, textures).clicked() && !is_busy {
                    intents.push(Intent::Submit(Action::OpenWorld(world.id.clone())));
                }
                ui.add_space(6.0);
            }
            ui.add_space(24.0);
        });
    });

    // While a file is dragged over the window, say what will happen.
    if ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) {
        let rect = ui.max_rect();
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_hint")));
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(170));
        let text = if is_busy {
            "Busy; wait or cancel first"
        } else {
            "Drop to open this photo"
        };
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(28.0),
            Color32::WHITE,
        );
    }
    intents
}

/// One clickable row: thumbnail, title, last update, frame count.
fn world_row(ui: &mut Ui, world: &WorldSummary, textures: &mut TextureCache) -> egui::Response {
    let ctx = ui.ctx().clone();
    ui.scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
        let hovered = ui.response().hovered();
        let fill = if hovered {
            ui.visuals().widgets.hovered.weak_bg_fill
        } else {
            ui.visuals().faint_bg_color
        };
        egui::Frame::new()
            .fill(fill)
            .corner_radius(6.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let thumb = world.thumbnail.as_deref().and_then(|p| textures.thumbnail(&ctx, p));
                    match thumb {
                        Some(t) => {
                            ui.add(
                                egui::Image::from_texture(t)
                                    .fit_to_exact_size(THUMB_SIZE)
                                    .maintain_aspect_ratio(true),
                            );
                        }
                        None => {
                            let (rect, _) = ui.allocate_exact_size(THUMB_SIZE, Sense::hover());
                            ui.painter().rect_filled(rect, 4.0, Color32::from_gray(40));
                        }
                    }
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        let title = if world.title.is_empty() {
                            "Untitled world"
                        } else {
                            world.title.as_str()
                        };
                        ui.label(RichText::new(title).size(16.0).strong());
                        ui.weak(format!("Updated {}", format_updated(&world.updated_at)));
                        let frames = if world.node_count == 1 {
                            "1 frame".to_string()
                        } else {
                            format!("{} frames", world.node_count)
                        };
                        ui.weak(frames);
                    });
                });
            });
    })
    .response
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Native file dialog, on the main thread (blocks until closed).
fn pick_photo() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open a photo")
        .add_filter("Images", &IMAGE_EXTENSIONS)
        .pick_file()
}

/// The first file with a path dropped on the window this frame, as an
/// OpenPhoto, or a message explaining why it was not opened.
fn dropped_photo(ui: &Ui, busy: bool) -> Option<Intent> {
    let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
    let path = dropped
        .iter()
        .map(|f| f.path().to_path_buf())
        .find(|p| !p.as_os_str().is_empty())?;
    if busy {
        return Some(Intent::Notify {
            text: "Busy; wait or cancel before opening another photo.".into(),
            is_error: true,
        });
    }
    if !is_image_path(&path) {
        return Some(Intent::Notify {
            text: format!(
                "Not a supported image ({}): {}",
                IMAGE_EXTENSIONS.join(", "),
                path.display()
            ),
            is_error: true,
        });
    }
    Some(Intent::Submit(Action::OpenPhoto(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_by_extension() {
        assert!(is_image_path(Path::new("/a/b/photo.JPG")));
        assert!(is_image_path(Path::new("x.webp")));
        assert!(is_image_path(Path::new("x.png")));
        assert!(!is_image_path(Path::new("x.gif")));
        assert!(!is_image_path(Path::new("noext")));
    }

    #[test]
    fn updated_time_formats_or_passes_through() {
        let s = format_updated("2026-10-09T20:41:00Z");
        assert!(s.contains("2026"), "{s}");
        assert_eq!(format_updated("yesterday"), "yesterday");
    }

    #[test]
    fn only_x_does_anything() {
        assert_eq!(map_key(Key::X, Modifiers::NONE), Some(Intent::Cancel));
        assert_eq!(map_key(Key::X, Modifiers::COMMAND), None);
        assert_eq!(map_key(Key::W, Modifiers::NONE), None);
    }
}
