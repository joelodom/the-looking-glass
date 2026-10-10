//! The Escape overlay: every control, the settings form, and the app's data
//! directory. Edits go to a draft; Save writes it to disk and makes it the
//! copy that travels with every engine submit.

use crate::app::{Intent, Message};
use crate::status_bar::message_row;
use egui::{Context, DragValue, Grid, Id, RichText, ScrollArea, Ui};
use iz_types::settings::paths;
use iz_types::{Quality, Settings};
use std::collections::VecDeque;

#[derive(Default)]
pub struct HelpState {
    pub open: bool,
    /// Unsaved edits; None means "same as the saved settings". Kept across
    /// close/open so a stray Escape does not lose them.
    draft: Option<Settings>,
}

/// Frame sizes are multiples of 16 (the image API's granularity).
pub fn round_to_16(v: u32) -> u32 {
    (v.saturating_add(8) / 16 * 16).clamp(256, 4096)
}

/// The control list as (keys, what they do) pairs, using the saved steps.
pub fn controls(s: &Settings) -> Vec<(String, String)> {
    let c = |k: &str, v: String| (k.to_string(), v);
    vec![
        c("Drag", "zoom into the rectangle (it snaps to the frame's shape)".into()),
        c("Click an outline", "go back into a frame you already made".into()),
        c("← / →", format!("turn left / right (heading −/+{:.0}°)", s.heading_step_deg)),
        c(
            "↑ / ↓",
            format!(
                "tilt up toward the horizon / down toward straight down (tilt +/−{:.0}°; 0° = straight down, 90° = horizon)",
                s.tilt_step_deg
            ),
        ),
        c("W", "step forward; if a wall, door or similar fills the view, the step goes through it".into()),
        c("Z", format!("zoom out ×{}", s.zoom_out_factor)),
        c("Home", "look straight down, facing north".into()),
        c("Backspace", "back to the previous frame".into()),
        c("Shift+Backspace", "redo (forward again after Back)".into()),
        c("R", "regenerate this frame (asks first if it has branches; those are discarded)".into()),
        c("X", "cancel the running generation".into()),
        c("F", "toggle fullscreen".into()),
        c("Esc", "this screen".into()),
    ]
}

pub fn show(
    ctx: &Context,
    state: &mut HelpState,
    saved: &Settings,
    can_leave_world: bool,
    messages: &VecDeque<Message>,
) -> Vec<Intent> {
    let mut intents = Vec::new();
    let max_height = (ctx.content_rect().height() - 120.0).max(200.0);
    let modal = egui::Modal::new(Id::new("help_overlay")).show(ctx, |ui| {
        ui.set_width(640.0_f32.min(ctx.content_rect().width() - 60.0));
        ui.horizontal(|ui| {
            ui.heading("Help & settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    intents.push(Intent::CloseHelp);
                }
            });
        });
        ui.separator();
        ScrollArea::vertical()
            .max_height(max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                controls_section(ui, saved);
                ui.add_space(12.0);
                ui.separator();
                intents.extend(settings_section(ui, state, saved));
                ui.add_space(12.0);
                ui.separator();
                ui.label(RichText::new("Data").strong());
                ui.horizontal_wrapped(|ui| {
                    ui.label("Worlds, settings and the image-call ledger live in");
                    ui.monospace(paths::app_dir().display().to_string());
                });
                if !messages.is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Recent messages").strong());
                    for m in messages.iter().rev() {
                        message_row(ui, m);
                    }
                }
            });
        ui.separator();
        ui.horizontal(|ui| {
            let back = ui.add_enabled(can_leave_world, egui::Button::new("Back to open screen"));
            let back = if can_leave_world {
                back
            } else {
                back.on_disabled_hover_text("Only from a world, when nothing is running")
            };
            if back.clicked() {
                intents.push(Intent::GoToOpenScreen);
            }
            if ui.button("Quit").clicked() {
                intents.push(Intent::Quit);
            }
        });
    });
    if modal.should_close() {
        intents.push(Intent::CloseHelp);
    }
    intents
}

fn controls_section(ui: &mut Ui, s: &Settings) {
    ui.label(RichText::new("Controls").strong());
    Grid::new("controls_grid")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for (keys, what) in controls(s) {
                ui.monospace(keys);
                ui.label(what);
                ui.end_row();
            }
        });
    ui.add_space(4.0);
    ui.weak("While a frame is being generated, only Esc and X work.");
}

fn settings_section(ui: &mut Ui, state: &mut HelpState, saved: &Settings) -> Vec<Intent> {
    let mut intents = Vec::new();
    ui.label(RichText::new("Settings").strong());
    ui.weak("Settings apply to frames generated from now on; existing frames stay as they are.");
    ui.add_space(4.0);
    let draft = state.draft.get_or_insert_with(|| saved.clone());
    Grid::new("settings_grid")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label("Image quality");
            egui::ComboBox::from_id_salt("quality")
                .selected_text(draft.image_quality.as_str())
                .show_ui(ui, |ui| {
                    for q in Quality::ALL {
                        ui.selectable_value(&mut draft.image_quality, q, q.as_str());
                    }
                });
            ui.end_row();

            ui.label("Image model");
            ui.text_edit_singleline(&mut draft.image_model);
            ui.end_row();

            ui.label("Claude model");
            ui.text_edit_singleline(&mut draft.claude_model);
            ui.end_row();

            ui.label("Frame size (px)");
            ui.horizontal(|ui| {
                frame_dim(ui, &mut draft.frame_width);
                ui.label("×");
                frame_dim(ui, &mut draft.frame_height);
            });
            ui.end_row();

            ui.label("Zoom-out factor");
            ui.add(
                DragValue::new(&mut draft.zoom_out_factor)
                    .range(1.5..=16.0)
                    .speed(0.1)
                    .prefix("×"),
            );
            ui.end_row();

            ui.label("Tilt step");
            ui.add(
                DragValue::new(&mut draft.tilt_step_deg)
                    .range(1.0..=90.0)
                    .speed(1.0)
                    .suffix("°"),
            );
            ui.end_row();

            ui.label("Heading step");
            ui.add(
                DragValue::new(&mut draft.heading_step_deg)
                    .range(1.0..=180.0)
                    .speed(1.0)
                    .suffix("°"),
            );
            ui.end_row();

            ui.label("claude binary");
            let mut bin = draft.claude_bin.clone().unwrap_or_default();
            let resp = ui.add(egui::TextEdit::singleline(&mut bin).hint_text("found automatically"));
            if resp.changed() {
                draft.claude_bin = if bin.trim().is_empty() { None } else { Some(bin) };
            }
            ui.end_row();
        });

    let dirty = draft != saved;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
            intents.push(Intent::SaveSettings(draft.clone()));
        }
        if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
            *draft = saved.clone();
        }
        if ui.button("Defaults").clicked() {
            *draft = Settings::default();
        }
        if dirty {
            ui.colored_label(ui.visuals().warn_fg_color, "Unsaved changes");
        }
    });
    // A saved or reverted draft is the saved settings again.
    if !dirty {
        state.draft = None;
    }
    intents
}

/// A frame dimension, snapped to a multiple of 16 once editing ends.
fn frame_dim(ui: &mut Ui, v: &mut u32) {
    let resp = ui.add(DragValue::new(v).range(256..=4096).speed(16.0));
    if !resp.dragged() && !resp.has_focus() {
        *v = round_to_16(*v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_dims_round_to_16() {
        assert_eq!(round_to_16(1536), 1536);
        assert_eq!(round_to_16(1530), 1536);
        assert_eq!(round_to_16(1543), 1536);
        assert_eq!(round_to_16(1544), 1552);
        assert_eq!(round_to_16(10), 256);
        assert_eq!(round_to_16(u32::MAX), 4096);
    }

    #[test]
    fn controls_list_every_key_and_the_tilt_direction() {
        let list = controls(&Settings::default());
        let keys: Vec<&str> = list.iter().map(|(k, _)| k.as_str()).collect();
        for k in [
            "Drag",
            "← / →",
            "↑ / ↓",
            "W",
            "Z",
            "Home",
            "Backspace",
            "Shift+Backspace",
            "R",
            "X",
            "F",
            "Esc",
        ] {
            assert!(keys.contains(&k), "missing {k}");
        }
        let tilt = &list.iter().find(|(k, _)| k == "↑ / ↓").unwrap().1;
        assert!(tilt.contains("up toward the horizon"), "{tilt}");
    }
}
