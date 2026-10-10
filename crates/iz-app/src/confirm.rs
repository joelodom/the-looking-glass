//! The "are you sure?" modal shown before R discards frames made from the
//! current one. Enter or the Regenerate button proceeds; Escape, a click
//! outside, or the Keep button leaves everything as it is.

use crate::app::Intent;
use egui::{Context, Id, Key, RichText};
use iz_types::{Action, WorldView};

/// Regenerating a frame with branches needs a confirmation; a leaf frame
/// regenerates straight away.
pub fn needs_confirmation(view: Option<&WorldView>) -> bool {
    view.is_some_and(|v| !v.children.is_empty())
}

pub fn show(ctx: &Context, branches: usize) -> Vec<Intent> {
    let mut intents = Vec::new();
    let modal = egui::Modal::new(Id::new("confirm_regenerate")).show(ctx, |ui| {
        ui.set_width(460.0_f32.min(ctx.content_rect().width() - 60.0));
        ui.heading("Regenerate this frame?");
        ui.add_space(6.0);
        let plural = if branches == 1 { "branch" } else { "branches" };
        ui.label(format!(
            "This frame has {branches} {plural}. Regenerating replaces its image and \
             discards every frame made from it. The files stay on disk, but the \
             branches are gone from this world."
        ));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Regenerate").strong()).clicked() {
                intents.push(Intent::RegenerateConfirmed);
            }
            if ui.button("Keep").clicked() {
                intents.push(Intent::CloseConfirm);
            }
            ui.label(RichText::new("Enter to regenerate · Esc to keep").weak());
        });
    });
    if modal.should_close() {
        intents.push(Intent::CloseConfirm);
    }
    intents
}

/// Keys while the modal is open. Everything else is swallowed.
pub fn map_key(key: Key) -> Option<Intent> {
    match key {
        Key::Enter | Key::Y => Some(Intent::RegenerateConfirmed),
        Key::N => Some(Intent::CloseConfirm),
        _ => None,
    }
}

/// The action the modal stands in for.
pub const ACTION: Action = Action::Regenerate;
