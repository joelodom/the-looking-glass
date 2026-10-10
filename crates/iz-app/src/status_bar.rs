//! The bottom status bar: the call in flight (service, purpose, step,
//! elapsed time) or, when idle, the last message; plus the breadcrumb.

use crate::app::{Intent, Message};
use egui::{Align, Layout, RichText, Ui};
use iz_types::{BusyInfo, WorldView};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// "m:ss", e.g. 0:07, 1:23, 12:05.
pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// "<Service> · <purpose> · <step> · <m:ss>"; empty parts are skipped.
pub fn busy_line(b: &BusyInfo, now: Instant) -> String {
    let elapsed = format_elapsed(now.saturating_duration_since(b.since));
    [b.service.name(), b.purpose.as_str(), b.step.as_str(), elapsed.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// "<title> · <breadcrumb>", or whichever is non-empty.
pub fn view_line(view: &WorldView) -> String {
    match (view.title.is_empty(), view.breadcrumb.is_empty()) {
        (false, false) => format!("{} · {}", view.title, view.breadcrumb),
        (false, true) => view.title.clone(),
        (true, false) => view.breadcrumb.clone(),
        (true, true) => view.world_title.clone(),
    }
}

/// Draw the bar. `view` is None on the open screen.
pub fn show(
    ui: &mut Ui,
    busy: Option<&BusyInfo>,
    view: Option<&WorldView>,
    messages: &VecDeque<Message>,
) -> Option<Intent> {
    let mut intent = None;
    ui.horizontal(|ui| {
        if let Some(b) = busy {
            ui.spinner();
            ui.label(RichText::new(busy_line(b, Instant::now())).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Cancel (X)").clicked() {
                    intent = Some(Intent::Cancel);
                }
            });
        } else if let Some(m) = messages.back() {
            let color = if m.is_error {
                ui.visuals().warn_fg_color
            } else {
                ui.visuals().text_color()
            };
            let resp = ui.label(RichText::new(&m.text).color(color));
            if messages.len() > 1 {
                resp.on_hover_ui(|ui| {
                    for m in messages.iter().rev() {
                        message_row(ui, m);
                    }
                });
            }
        } else {
            ui.weak("Ready.");
        }
    });
    ui.horizontal(|ui| {
        if let Some(v) = view {
            ui.label(RichText::new(view_line(v)).weak());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.weak("Esc: help & settings");
        });
    });
    intent
}

/// One message with its time, errors in the warning color.
pub fn message_row(ui: &mut Ui, m: &Message) {
    let color = if m.is_error {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().text_color()
    };
    ui.horizontal(|ui| {
        ui.weak(m.at.format("%H:%M:%S").to_string());
        ui.label(RichText::new(&m.text).color(color));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use iz_types::Service;

    #[test]
    fn elapsed_is_minutes_and_padded_seconds() {
        assert_eq!(format_elapsed(Duration::from_millis(0)), "0:00");
        assert_eq!(format_elapsed(Duration::from_millis(7_900)), "0:07");
        assert_eq!(format_elapsed(Duration::from_secs(83)), "1:23");
        assert_eq!(format_elapsed(Duration::from_secs(725)), "12:05");
    }

    #[test]
    fn busy_line_joins_parts() {
        let since = Instant::now();
        let b = BusyInfo {
            service: Service::OpenAI,
            purpose: "paint zoom in".into(),
            since,
            step: "2/2 paint".into(),
        };
        let line = busy_line(&b, since + Duration::from_secs(65));
        assert_eq!(line, "OpenAI · paint zoom in · 2/2 paint · 1:05");
        let b = BusyInfo {
            step: String::new(),
            ..b
        };
        assert_eq!(busy_line(&b, since), "OpenAI · paint zoom in · 0:00");
    }
}
