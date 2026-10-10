//! The main screen: the current frame letterboxed in the window, drag to
//! zoom in, outlines of frames already made from this one, and the keys.

use crate::app::Intent;
use crate::textures::TextureCache;
use egui::{
    pos2, vec2, Align2, Color32, FontId, Key, Modifiers, PointerButton, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2,
};
use iz_types::imageops::snap_rect_to_aspect;
use iz_types::{Action, ChildOutline, NodeId, RectF, Settings, Transition, WorldView};

/// A drag smaller than this (in points, on its shorter side) is treated as
/// an accidental click, not a zoom.
const MIN_DRAG_SHORT_SIDE: f32 = 6.0;
/// ...and the longer side must be at least this.
const MIN_DRAG_LONG_SIDE: f32 = 10.0;

const DIM: Color32 = Color32::from_black_alpha(140);
const OUTLINE: Color32 = Color32::from_rgba_premultiplied(70, 70, 70, 70);
const OUTLINE_HOVER: Color32 = Color32::from_rgba_premultiplied(200, 200, 200, 200);

#[derive(Default)]
pub struct MainState {
    /// Where the current drag started, normalized to the image.
    drag_anchor: Option<(f32, f32)>,
}

/// Map a key press to an intent. Escape is handled by the app.
///
/// Up tilts toward the horizon (tilt increases, 90 = horizon) and Down
/// toward straight down (tilt decreases, 0 = straight down). Left turns
/// counterclockwise (heading decreases), Right clockwise.
/// Keys with Cmd/Ctrl/Alt are left alone so system shortcuts keep working.
pub fn map_key(key: Key, mods: Modifiers, s: &Settings) -> Option<Intent> {
    if mods.command || mods.ctrl || mods.alt || mods.mac_cmd {
        return None;
    }
    let submit = |a: Action| Some(Intent::Submit(a));
    match key {
        Key::ArrowLeft => submit(Action::Rotate {
            delta_deg: -s.heading_step_deg,
        }),
        Key::ArrowRight => submit(Action::Rotate {
            delta_deg: s.heading_step_deg,
        }),
        Key::ArrowUp => submit(Action::Tilt {
            delta_deg: s.tilt_step_deg,
        }),
        Key::ArrowDown => submit(Action::Tilt {
            delta_deg: -s.tilt_step_deg,
        }),
        Key::W => submit(Action::Forward),
        Key::S => submit(Action::Backward),
        Key::Z => submit(Action::ZoomOut),
        Key::Home => submit(Action::Home),
        Key::Backspace if mods.shift => submit(Action::Redo),
        Key::Backspace => submit(Action::Back),
        Key::R => submit(Action::Regenerate),
        Key::X => Some(Intent::Cancel),
        Key::F => Some(Intent::ToggleFullscreen),
        _ => None,
    }
}

pub fn show(
    ui: &mut Ui,
    state: &mut MainState,
    view: Option<&WorldView>,
    busy: bool,
    settings: &Settings,
    textures: &mut TextureCache,
) -> Vec<Intent> {
    let mut intents = Vec::new();
    let area = ui.max_rect();
    let Some(view) = view else {
        centered_message(ui, area, "No world is open.");
        let button_rect = Rect::from_center_size(area.center() + vec2(0.0, 32.0), vec2(200.0, 28.0));
        if ui.put(button_rect, egui::Button::new("Back to open screen")).clicked() {
            intents.push(Intent::GoToOpenScreen);
        }
        return intents;
    };
    let ctx = ui.ctx().clone();
    let (tex_id, img_w, img_h) = match textures.frame(&ctx, view) {
        Ok(t) => (t.handle.id(), t.width, t.height),
        Err(e) => {
            centered_message(ui, area, &format!("Cannot show this frame: {e}"));
            return intents;
        }
    };
    let img_rect = fit_rect(area, vec2(img_w as f32, img_h as f32));
    let painter = ui.painter_at(area);
    painter.image(
        tex_id,
        img_rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    let response = ui.interact(img_rect, ui.id().with("frame"), Sense::click_and_drag());
    if busy {
        state.drag_anchor = None;
    }

    // Outlines of existing ZoomIn children.
    let zoom_children: Vec<(&ChildOutline, RectF)> = view
        .children
        .iter()
        .filter_map(|c| match &c.transition {
            Transition::ZoomIn { rect } => Some((c, *rect)),
            _ => None,
        })
        .collect();
    let hovered = if state.drag_anchor.is_none() && !busy {
        response.hover_pos().and_then(|p| {
            let (x, y) = screen_to_norm(p, img_rect);
            hit_outline(&view.children, x, y)
        })
    } else {
        None
    };
    for (child, rect) in &zoom_children {
        let r = norm_to_screen(*rect, img_rect);
        let is_hovered = hovered == Some(child.node);
        let color = if is_hovered { OUTLINE_HOVER } else { OUTLINE };
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
        if is_hovered {
            label_with_background(&painter, r.left_top() + vec2(4.0, 4.0), &outline_label(child));
        }
    }
    if hovered.is_some() {
        response.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
    }

    // Drag to zoom.
    if !busy && response.drag_started_by(PointerButton::Primary) {
        let origin = ctx
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos());
        state.drag_anchor = origin.map(|p| screen_to_norm(p, img_rect));
    }
    if let Some((ax, ay)) = state.drag_anchor {
        let pointer = ctx
            .input(|i| i.pointer.latest_pos())
            .or(response.interact_pointer_pos());
        if let Some(p) = pointer {
            let (bx, by) = screen_to_norm(p, img_rect);
            let snapped = snap_rect_to_aspect(ax, ay, bx, by, img_w, img_h, settings.frame_aspect());
            let sel = norm_to_screen(snapped, img_rect);
            draw_selection(&painter, img_rect, sel, snapped);
            if response.drag_stopped() {
                state.drag_anchor = None;
                if is_meaningful_drag(sel.size()) {
                    intents.push(Intent::Submit(Action::ZoomIn(snapped)));
                }
            }
        }
        if !response.dragged() && !response.drag_stopped() {
            // The drag ended without us seeing the release (e.g. focus loss).
            state.drag_anchor = None;
        }
    }

    // Click inside an outline re-enters that branch.
    if !busy && response.clicked_by(PointerButton::Primary) {
        if let Some(p) = response.interact_pointer_pos() {
            let (x, y) = screen_to_norm(p, img_rect);
            if let Some(node) = hit_outline(&view.children, x, y) {
                intents.push(Intent::Submit(Action::EnterChild(node)));
            }
        }
    }

    // Other transitions (zoom out, look, forward) as a small clickable list.
    let others: Vec<&ChildOutline> = view
        .children
        .iter()
        .filter(|c| !matches!(c.transition, Transition::ZoomIn { .. }))
        .collect();
    if !others.is_empty() {
        let list_rect = Rect::from_min_size(img_rect.left_top() + vec2(8.0, 8.0), vec2(360.0, img_rect.height()));
        ui.scope_builder(UiBuilder::new().max_rect(list_rect), |ui| {
            egui::Frame::new()
                .fill(Color32::from_black_alpha(150))
                .corner_radius(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Already made from here:")
                            .small()
                            .color(Color32::from_gray(170)),
                    );
                    for child in others {
                        let text = RichText::new(outline_label(child))
                            .small()
                            .color(Color32::from_gray(220));
                        let resp = ui.add(egui::Label::new(text).sense(Sense::click()).truncate());
                        if resp.clicked() && !busy {
                            intents.push(Intent::Submit(Action::EnterChild(child.node)));
                        }
                        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    }
                });
        });
    }
    intents
}

fn outline_label(c: &ChildOutline) -> String {
    if c.title.is_empty() {
        c.transition.label()
    } else {
        format!("{} · {}", c.transition.label(), c.title)
    }
}

fn centered_message(ui: &Ui, area: Rect, text: &str) {
    ui.painter().text(
        area.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(16.0),
        Color32::from_gray(200),
    );
}

fn label_with_background(painter: &egui::Painter, pos: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(13.0), Color32::WHITE);
    let rect = Rect::from_min_size(pos, galley.size()).expand(3.0);
    painter.rect_filled(rect, 3.0, Color32::from_black_alpha(170));
    painter.galley(pos, galley, Color32::WHITE);
}

/// Dim everything outside the selection, outline it and show the zoom factor.
fn draw_selection(painter: &egui::Painter, img: Rect, sel: Rect, norm: RectF) {
    let sel = sel.intersect(img);
    for r in [
        Rect::from_min_max(img.min, pos2(img.max.x, sel.min.y)),
        Rect::from_min_max(pos2(img.min.x, sel.max.y), img.max),
        Rect::from_min_max(pos2(img.min.x, sel.min.y), pos2(sel.min.x, sel.max.y)),
        Rect::from_min_max(pos2(sel.max.x, sel.min.y), pos2(img.max.x, sel.max.y)),
    ] {
        if r.is_positive() {
            painter.rect_filled(r, 0.0, DIM);
        }
    }
    painter.rect_stroke(sel, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
    if is_meaningful_drag(sel.size()) {
        let factor = 1.0 / norm.w.max(1e-6);
        label_with_background(painter, sel.left_top() + vec2(4.0, 4.0), &format!("×{factor:.1}"));
    }
}

/// The largest rect of the image's aspect that fits in `area`, centered
/// (letterboxed or pillarboxed).
pub fn fit_rect(area: Rect, img: Vec2) -> Rect {
    if img.x <= 0.0 || img.y <= 0.0 || !area.is_positive() {
        return area;
    }
    let scale = (area.width() / img.x).min(area.height() / img.y);
    Rect::from_center_size(area.center(), img * scale)
}

/// Screen point to normalized image coordinates, clamped to the image.
pub fn screen_to_norm(p: Pos2, img: Rect) -> (f32, f32) {
    let x = ((p.x - img.min.x) / img.width().max(1e-6)).clamp(0.0, 1.0);
    let y = ((p.y - img.min.y) / img.height().max(1e-6)).clamp(0.0, 1.0);
    (x, y)
}

/// Normalized image rect to screen rect.
pub fn norm_to_screen(r: RectF, img: Rect) -> Rect {
    Rect::from_min_size(
        pos2(img.min.x + r.x * img.width(), img.min.y + r.y * img.height()),
        vec2(r.w * img.width(), r.h * img.height()),
    )
}

/// The ZoomIn child whose rect contains the point; the smallest one if
/// several overlap (a nested outline wins over its surroundings).
pub fn hit_outline(children: &[ChildOutline], x: f32, y: f32) -> Option<NodeId> {
    children
        .iter()
        .filter_map(|c| match &c.transition {
            Transition::ZoomIn { rect } if rect.contains(x, y) => Some((c.node, rect.area())),
            _ => None,
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(node, _)| node)
}

/// True when a drag's snapped rect (in points) is big enough to be a zoom
/// rather than an accidental click.
pub fn is_meaningful_drag(size: Vec2) -> bool {
    size.x.min(size.y) >= MIN_DRAG_SHORT_SIDE && size.x.max(size.y) >= MIN_DRAG_LONG_SIDE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit(intent: Option<Intent>) -> Action {
        match intent {
            Some(Intent::Submit(a)) => a,
            other => panic!("expected a submit, got {other:?}"),
        }
    }

    #[test]
    fn every_key_maps_to_the_documented_action() {
        let s = Settings::default();
        let none = Modifiers::NONE;
        assert_eq!(
            submit(map_key(Key::ArrowLeft, none, &s)),
            Action::Rotate { delta_deg: -45.0 }
        );
        assert_eq!(
            submit(map_key(Key::ArrowRight, none, &s)),
            Action::Rotate { delta_deg: 45.0 }
        );
        assert_eq!(
            submit(map_key(Key::ArrowUp, none, &s)),
            Action::Tilt { delta_deg: 30.0 }
        );
        assert_eq!(
            submit(map_key(Key::ArrowDown, none, &s)),
            Action::Tilt { delta_deg: -30.0 }
        );
        assert_eq!(submit(map_key(Key::W, none, &s)), Action::Forward);
        assert_eq!(submit(map_key(Key::S, none, &s)), Action::Backward);
        assert_eq!(submit(map_key(Key::Z, none, &s)), Action::ZoomOut);
        assert_eq!(submit(map_key(Key::Home, none, &s)), Action::Home);
        assert_eq!(submit(map_key(Key::Backspace, none, &s)), Action::Back);
        assert_eq!(submit(map_key(Key::Backspace, Modifiers::SHIFT, &s)), Action::Redo);
        assert_eq!(submit(map_key(Key::R, none, &s)), Action::Regenerate);
        assert_eq!(map_key(Key::X, none, &s), Some(Intent::Cancel));
        assert_eq!(map_key(Key::F, none, &s), Some(Intent::ToggleFullscreen));
        assert_eq!(map_key(Key::Q, none, &s), None);
    }

    #[test]
    fn steps_come_from_settings() {
        let s = Settings {
            heading_step_deg: 10.0,
            tilt_step_deg: 5.0,
            ..Settings::default()
        };
        assert_eq!(
            submit(map_key(Key::ArrowLeft, Modifiers::NONE, &s)),
            Action::Rotate { delta_deg: -10.0 }
        );
        assert_eq!(
            submit(map_key(Key::ArrowDown, Modifiers::NONE, &s)),
            Action::Tilt { delta_deg: -5.0 }
        );
    }

    #[test]
    fn system_shortcuts_are_ignored() {
        let s = Settings::default();
        assert_eq!(map_key(Key::W, Modifiers::COMMAND, &s), None);
        assert_eq!(map_key(Key::Z, Modifiers::CTRL, &s), None);
        assert_eq!(map_key(Key::R, Modifiers::ALT, &s), None);
    }

    #[test]
    fn fit_rect_letterboxes_and_pillarboxes() {
        // Wide area, 3:2 image: pillarbox (bars left and right).
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(2000.0, 1000.0));
        let r = fit_rect(area, vec2(1536.0, 1024.0));
        assert!((r.height() - 1000.0).abs() < 1e-3);
        assert!((r.width() - 1500.0).abs() < 1e-3);
        assert!((r.center().x - 1000.0).abs() < 1e-3);
        // Tall area: letterbox (bars top and bottom).
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(900.0, 1200.0));
        let r = fit_rect(area, vec2(1536.0, 1024.0));
        assert!((r.width() - 900.0).abs() < 1e-3);
        assert!((r.height() - 600.0).abs() < 1e-3);
        assert!((r.min.y - (20.0 + 300.0)).abs() < 1e-3);
    }

    #[test]
    fn screen_norm_round_trip() {
        let img = Rect::from_min_size(pos2(100.0, 50.0), vec2(600.0, 400.0));
        assert_eq!(screen_to_norm(pos2(100.0, 50.0), img), (0.0, 0.0));
        assert_eq!(screen_to_norm(pos2(700.0, 450.0), img), (1.0, 1.0));
        assert_eq!(screen_to_norm(pos2(400.0, 250.0), img), (0.5, 0.5));
        // Outside the image clamps to the edge.
        assert_eq!(screen_to_norm(pos2(0.0, 1000.0), img), (0.0, 1.0));
        let r = RectF::new(0.25, 0.5, 0.5, 0.25);
        let s = norm_to_screen(r, img);
        assert_eq!(s, Rect::from_min_size(pos2(250.0, 250.0), vec2(300.0, 100.0)));
        let (x, y) = screen_to_norm(s.min, img);
        assert!((x - r.x).abs() < 1e-6 && (y - r.y).abs() < 1e-6);
    }

    #[test]
    fn hit_picks_smallest_containing_zoom_outline() {
        let outline = |node, t| ChildOutline {
            node,
            transition: t,
            title: String::new(),
        };
        let children = vec![
            outline(
                1,
                Transition::ZoomIn {
                    rect: RectF::new(0.0, 0.0, 0.8, 0.8),
                },
            ),
            outline(
                2,
                Transition::ZoomIn {
                    rect: RectF::new(0.2, 0.2, 0.2, 0.2),
                },
            ),
            outline(3, Transition::ZoomOut { factor: 4.0 }),
        ];
        assert_eq!(hit_outline(&children, 0.3, 0.3), Some(2));
        assert_eq!(hit_outline(&children, 0.7, 0.7), Some(1));
        assert_eq!(hit_outline(&children, 0.9, 0.9), None);
    }

    #[test]
    fn tiny_drags_are_clicks() {
        assert!(!is_meaningful_drag(vec2(3.0, 2.0)));
        assert!(!is_meaningful_drag(vec2(9.0, 6.0)));
        assert!(is_meaningful_drag(vec2(15.0, 10.0)));
        assert!(is_meaningful_drag(vec2(300.0, 200.0)));
    }
}

/// Drives `show` headlessly through an egui Context with synthetic pointer
/// input, to check the drag and click paths end to end.
#[cfg(test)]
mod headless {
    use super::*;
    use egui::{Context, Event, RawInput};

    struct Rig {
        ctx: Context,
        state: MainState,
        textures: TextureCache,
        view: WorldView,
        settings: Settings,
        time: f64,
    }

    impl Rig {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("iz-app-test-{}-{name}.png", std::process::id()));
            let img = image::RgbaImage::from_pixel(300, 200, image::Rgba([90, 120, 150, 255]));
            iz_types::imageops::save_png(&img, &path).unwrap();
            let view = WorldView {
                world_id: "w".into(),
                world_title: "W".into(),
                current: 1,
                image_path: path,
                image_version: 0,
                camera: iz_types::Camera::default(),
                title: "t".into(),
                breadcrumb: "b".into(),
                children: vec![
                    ChildOutline {
                        node: 7,
                        transition: Transition::ZoomIn {
                            rect: RectF::new(0.5, 0.5, 0.25, 0.25),
                        },
                        title: "seven".into(),
                    },
                    ChildOutline {
                        node: 8,
                        transition: Transition::ZoomOut { factor: 4.0 },
                        title: "eight".into(),
                    },
                ],
                can_back: true,
                can_redo: false,
                depth: 1,
            };
            Self {
                ctx: Context::default(),
                state: MainState::default(),
                textures: TextureCache::default(),
                view,
                settings: Settings::default(),
                time: 0.0,
            }
        }

        /// One frame on a 1000x800 screen; the 300x200 image letterboxes to
        /// x 0..1000, y 66.7..733.3.
        fn frame(&mut self, events: Vec<Event>, busy: bool) -> Vec<Intent> {
            self.time += 1.0 / 60.0;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut intents = vec![];
            let Rig {
                ctx,
                state,
                textures,
                view,
                settings,
                ..
            } = self;
            ctx.run_ui(input, |ui| {
                intents.extend(show(ui, state, Some(view), busy, settings, textures))
            })
            .drop_without_applying_deltas();
            intents
        }

        fn gesture(&mut self, from: Pos2, to: Pos2, busy: bool) -> Vec<Intent> {
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            let mut out = self.frame(vec![], busy);
            out.extend(self.frame(vec![Event::PointerMoved(from)], busy));
            out.extend(self.frame(vec![button(from, true)], busy));
            for i in 1..=4 {
                let p = from + (to - from) * (i as f32 / 4.0);
                out.extend(self.frame(vec![Event::PointerMoved(p)], busy));
            }
            out.extend(self.frame(vec![button(to, false)], busy));
            out.extend(self.frame(vec![], busy));
            out
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.view.image_path);
        }
    }

    fn to_screen(x: f32, y: f32) -> Pos2 {
        pos2(x * 1000.0, 800.0 / 2.0 - 1000.0 / 3.0 + y * (2000.0 / 3.0))
    }

    #[test]
    fn drag_submits_a_snapped_zoom_in() {
        let mut rig = Rig::new("drag");
        let intents = rig.gesture(to_screen(0.1, 0.1), to_screen(0.3, 0.5), false);
        let zooms: Vec<RectF> = intents
            .iter()
            .filter_map(|i| match i {
                Intent::Submit(Action::ZoomIn(r)) => Some(*r),
                _ => None,
            })
            .collect();
        assert_eq!(zooms.len(), 1, "{intents:?}");
        let r = zooms[0];
        assert!((r.x - 0.1).abs() < 0.01 && (r.y - 0.1).abs() < 0.01, "{r:?}");
        // Dragged 0.2 x 0.4 of a 3:2 image at 3:2 target: width limits, so
        // the snapped rect is 0.2 x 0.2 (normalized), i.e. 3:2 in pixels.
        assert!((r.w - 0.2).abs() < 0.01 && (r.h - 0.2).abs() < 0.01, "{r:?}");
        assert!(rig.state.drag_anchor.is_none());
    }

    #[test]
    fn click_in_outline_enters_child_and_tiny_drag_does_nothing() {
        let mut rig = Rig::new("click");
        let p = to_screen(0.6, 0.6);
        let intents = rig.gesture(p, p, false);
        assert_eq!(intents, vec![Intent::Submit(Action::EnterChild(7))]);
        let intents = rig.gesture(to_screen(0.1, 0.1), to_screen(0.1, 0.1) + vec2(4.0, 3.0), false);
        assert!(intents.is_empty(), "{intents:?}");
    }

    #[test]
    fn busy_blocks_drag_and_click() {
        let mut rig = Rig::new("busy");
        assert!(rig.gesture(to_screen(0.1, 0.1), to_screen(0.4, 0.5), true).is_empty());
        let p = to_screen(0.6, 0.6);
        assert!(rig.gesture(p, p, true).is_empty());
    }
}
