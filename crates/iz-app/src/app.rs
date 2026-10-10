//! The eframe App: owns the engine, the saved settings and the screens.
//! Every frame it drains engine events, reads keys, draws the current screen
//! and then applies whatever the screens and keys asked for ([`Intent`]).

use crate::confirm;
use crate::help_screen::{self, HelpState};
use crate::main_screen::{self, MainState};
use crate::open_screen;
use crate::status_bar;
use crate::textures::TextureCache;
use egui::{Color32, Context, Event, Key, ViewportCommand};
use iz_types::{Action, Engine, EngineEvent, Settings, WorldSummary, WorldView};
use std::collections::VecDeque;
use std::time::Duration;

/// How many recent Info/Error messages to keep for display.
const MAX_MESSAGES: usize = 5;
/// Repaint cadence while a generation runs, so the elapsed timer advances.
const BUSY_REPAINT: Duration = Duration::from_millis(100);
/// The engine cannot wake the UI, so poll it slowly even when idle.
const IDLE_REPAINT: Duration = Duration::from_millis(500);
/// Preferred initial window size; shrunk to fit the screen.
pub const PREFERRED_WINDOW: [f32; 2] = [1600.0, 1100.0];

/// What a screen, a button or a key asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Hand an action to the engine with the saved settings.
    Submit(Action),
    Cancel,
    ToggleFullscreen,
    OpenHelp,
    CloseHelp,
    SaveSettings(Settings),
    /// The user confirmed regenerating a frame that has branches.
    RegenerateConfirmed,
    /// Close the regenerate confirmation without doing anything.
    CloseConfirm,
    GoToOpenScreen,
    /// Show a message in the status bar (errors detected by the UI itself).
    Notify {
        text: String,
        is_error: bool,
    },
    Quit,
}

impl Intent {
    /// Keys that still work while a generation runs (DESIGN.md: ESC and X).
    pub fn allowed_while_busy(&self) -> bool {
        matches!(self, Intent::Cancel | Intent::OpenHelp | Intent::CloseHelp | Intent::CloseConfirm)
    }
}

/// An Info or Error from the engine (or the UI), for the status bar.
#[derive(Debug, Clone)]
pub struct Message {
    pub text: String,
    pub is_error: bool,
    pub at: chrono::DateTime<chrono::Local>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Open,
    Main,
}

pub struct App {
    engine: Box<dyn Engine>,
    /// The saved settings; a clone goes with every submit.
    settings: Settings,
    screen: Screen,
    view: Option<WorldView>,
    worlds: Vec<WorldSummary>,
    messages: VecDeque<Message>,
    textures: TextureCache,
    main: MainState,
    help: HelpState,
    /// Open when R was pressed on a frame with branches: the branch count.
    confirm_regenerate: Option<usize>,
    window_fitted: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: Box<dyn Engine>, settings: Settings) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let worlds = engine.list_worlds();
        Self {
            engine,
            settings,
            screen: Screen::Open,
            view: None,
            worlds,
            messages: VecDeque::new(),
            textures: TextureCache::default(),
            main: MainState::default(),
            help: HelpState::default(),
            confirm_regenerate: None,
            window_fitted: false,
        }
    }

    fn push_message(&mut self, text: String, is_error: bool) {
        self.messages.push_back(Message {
            text,
            is_error,
            at: chrono::Local::now(),
        });
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
    }

    fn drain_events(&mut self) {
        for event in self.engine.poll() {
            match event {
                // `busy()` is what the status bar shows; nothing to keep here.
                EngineEvent::Status(_) => {}
                EngineEvent::ViewChanged => {
                    self.view = self.engine.view();
                    self.textures.retry_failed_frame();
                }
                EngineEvent::WorldOpened => {
                    self.view = self.engine.view();
                    self.textures.retry_failed_frame();
                    self.screen = Screen::Main;
                    self.main = MainState::default();
                }
                EngineEvent::WorldsChanged => {
                    self.worlds = self.engine.list_worlds();
                    self.textures.clear_thumbnails();
                }
                EngineEvent::Info(text) => self.push_message(text, false),
                EngineEvent::Error(text) => self.push_message(text, true),
            }
        }
    }

    /// Key presses this frame turned into intents. Escape only opens the
    /// overlay here; closing it is left to the modal (which consumes Escape).
    fn key_intents(&self, ctx: &Context, busy: bool) -> Vec<Intent> {
        let presses: Vec<(Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        let mut out = Vec::new();
        for (key, mods) in presses {
            if key == Key::Escape {
                // The modals close themselves on Escape.
                if !self.help.open && self.confirm_regenerate.is_none() && !egui::Popup::is_any_open(ctx) {
                    out.push(Intent::OpenHelp);
                }
                continue;
            }
            if ctx.egui_wants_keyboard_input() {
                continue;
            }
            if self.confirm_regenerate.is_some() {
                if mods.is_none() {
                    out.extend(confirm::map_key(key));
                }
                continue;
            }
            if self.help.open {
                // The overlay hides the Cancel button, so X still cancels.
                if key == Key::X && mods.is_none() {
                    out.push(Intent::Cancel);
                }
                continue;
            }
            let intent = match self.screen {
                Screen::Main => main_screen::map_key(key, mods, &self.settings),
                Screen::Open => open_screen::map_key(key, mods),
            };
            if let Some(intent) = intent {
                if !busy || intent.allowed_while_busy() {
                    out.push(intent);
                }
            }
        }
        out
    }

    fn apply(&mut self, ctx: &Context, intent: Intent) {
        match intent {
            Intent::Submit(confirm::ACTION) if confirm::needs_confirmation(self.view.as_ref()) => {
                let branches = self.view.as_ref().map_or(0, |v| v.children.len());
                self.confirm_regenerate = Some(branches);
            }
            Intent::RegenerateConfirmed => {
                self.confirm_regenerate = None;
                println!("[ui] submit {:?} (confirmed)", confirm::ACTION);
                if let Err(e) = self.engine.submit(confirm::ACTION, self.settings.clone()) {
                    self.push_message(e.to_string(), true);
                }
            }
            Intent::CloseConfirm => self.confirm_regenerate = None,
            Intent::Submit(action) => {
                println!("[ui] submit {action:?}");
                if let Err(e) = self.engine.submit(action, self.settings.clone()) {
                    self.push_message(e.to_string(), true);
                }
            }
            Intent::Cancel => self.engine.cancel(),
            Intent::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
            }
            Intent::OpenHelp => self.help.open = true,
            Intent::CloseHelp => self.help.open = false,
            Intent::SaveSettings(s) => match s.save() {
                Ok(()) => {
                    self.settings = s;
                    self.push_message(
                        "Settings saved; they apply to frames generated from now on.".into(),
                        false,
                    );
                }
                Err(e) => self.push_message(format!("Could not save settings: {e}"), true),
            },
            Intent::GoToOpenScreen => {
                self.screen = Screen::Open;
                self.help.open = false;
                self.worlds = self.engine.list_worlds();
                self.textures.clear_thumbnails();
            }
            Intent::Notify { text, is_error } => self.push_message(text, is_error),
            Intent::Quit => {
                self.engine.cancel();
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// Once the monitor size is known, shrink the window if it does not fit
    /// (eframe clamps to the full monitor, which ignores the menu bar/dock).
    fn fit_window_once(&mut self, ctx: &Context) {
        if self.window_fitted {
            return;
        }
        let (monitor, inner, fullscreen) = ctx.input(|i| {
            (
                i.viewport().monitor_size,
                i.viewport().inner_rect,
                i.viewport().fullscreen,
            )
        });
        let (Some(monitor), Some(inner)) = (monitor, inner) else {
            return;
        };
        self.window_fitted = true;
        if fullscreen == Some(true) {
            return;
        }
        let target = fit_window(PREFERRED_WINDOW, [monitor.x, monitor.y]);
        if inner.width() > target[0] + 1.0 || inner.height() > target[1] + 1.0 {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(target[0], target[1])));
        }
    }
}

/// The preferred size, shrunk to leave room for the menu bar, dock and title
/// bar on a monitor of the given size (in points); never below 800x600.
pub fn fit_window(preferred: [f32; 2], monitor: [f32; 2]) -> [f32; 2] {
    [
        preferred[0].min(monitor[0] - 40.0).max(800.0),
        preferred[1].min(monitor[1] - 120.0).max(600.0),
    ]
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        let busy = self.engine.busy().is_some();
        ctx.request_repaint_after(if busy { BUSY_REPAINT } else { IDLE_REPAINT });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fit_window_once(&ctx);
        let busy = self.engine.busy();
        let mut intents = self.key_intents(&ctx, busy.is_some());

        let status_view = if self.screen == Screen::Main {
            self.view.as_ref()
        } else {
            None
        };
        let status = egui::Panel::bottom("status_bar")
            .show(ui, |ui| {
                status_bar::show(ui, busy.as_ref(), status_view, &self.messages)
            })
            .inner;
        intents.extend(status);

        let fill = if self.screen == Screen::Main {
            Color32::BLACK
        } else {
            ui.visuals().panel_fill
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(fill))
            .show(ui, |ui| match self.screen {
                Screen::Open => intents.extend(open_screen::show(
                    ui,
                    &self.worlds,
                    busy.as_ref(),
                    &self.messages,
                    &mut self.textures,
                )),
                Screen::Main => intents.extend(main_screen::show(
                    ui,
                    &mut self.main,
                    self.view.as_ref(),
                    busy.is_some(),
                    &self.settings,
                    &mut self.textures,
                )),
            });

        if let Some(branches) = self.confirm_regenerate {
            intents.extend(confirm::show(&ctx, branches));
        }

        if self.help.open {
            let can_leave_world = self.screen == Screen::Main && busy.is_none();
            intents.extend(help_screen::show(
                &ctx,
                &mut self.help,
                &self.settings,
                can_leave_world,
                &self.messages,
            ));
        }

        for intent in intents {
            self.apply(&ctx, intent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cancel_and_help_pass_while_busy() {
        assert!(Intent::Cancel.allowed_while_busy());
        assert!(Intent::OpenHelp.allowed_while_busy());
        assert!(!Intent::ToggleFullscreen.allowed_while_busy());
        assert!(!Intent::Submit(Action::ZoomOut).allowed_while_busy());
        assert!(!Intent::Submit(Action::Back).allowed_while_busy());
    }

    #[test]
    fn regenerate_asks_only_when_there_are_branches() {
        assert!(!confirm::needs_confirmation(None));
        let mut view = WorldView {
            world_id: "w".into(),
            world_title: "w".into(),
            current: 1,
            image_path: std::path::PathBuf::from("x.png"),
            image_version: 0,
            camera: iz_types::Camera::default(),
            title: String::new(),
            breadcrumb: String::new(),
            children: vec![],
            can_back: false,
            can_redo: false,
            depth: 0,
        };
        assert!(!confirm::needs_confirmation(Some(&view)));
        view.children.push(iz_types::ChildOutline {
            node: 2,
            transition: iz_types::Transition::ZoomOut { factor: 4.0 },
            title: String::new(),
        });
        assert!(confirm::needs_confirmation(Some(&view)));
    }

    #[test]
    fn window_fits_small_and_large_screens() {
        assert_eq!(fit_window(PREFERRED_WINDOW, [2560.0, 1440.0]), [1600.0, 1100.0]);
        assert_eq!(fit_window(PREFERRED_WINDOW, [1470.0, 956.0]), [1430.0, 836.0]);
        assert_eq!(fit_window(PREFERRED_WINDOW, [700.0, 500.0]), [800.0, 600.0]);
    }
}

/// The whole App driven headlessly (no window) against the dev engine:
/// keys, busy gating, the help overlay, screens and engine events.
#[cfg(test)]
mod headless {
    use super::*;
    use crate::dev_engine::DevEngine;
    use eframe::App as _;
    use egui::{pos2, vec2, Modifiers, RawInput, Rect};
    use iz_types::Transition;

    struct Harness {
        ctx: Context,
        frame: eframe::Frame,
        app: App,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = Context::default();
            let cc = eframe::CreationContext::_new_kittest(ctx.clone());
            let settings = Settings {
                frame_width: 384,
                frame_height: 256,
                ..Settings::default()
            };
            let app = App::new(&cc, Box::new(DevEngine::new()), settings);
            Self {
                ctx,
                frame: eframe::Frame::_new_kittest(),
                app,
                time: 0.0,
            }
        }

        fn step(&mut self, events: Vec<Event>) {
            self.time += 1.0 / 60.0;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 900.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let Harness { ctx, frame, app, .. } = self;
            ctx.run_ui(input, |ui| {
                app.logic(ui.ctx(), frame);
                app.ui(ui, frame);
            })
            .drop_without_applying_deltas();
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            let ev = |pressed| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            self.step(vec![ev(true)]);
            self.step(vec![ev(false)]);
        }

        fn intent(&mut self, intent: Intent) {
            let ctx = self.ctx.clone();
            self.app.apply(&ctx, intent);
            self.step(vec![]);
        }

        fn wait_idle(&mut self) {
            for _ in 0..500 {
                if self.app.engine.busy().is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
                self.step(vec![]);
            }
            self.step(vec![]);
            self.step(vec![]);
        }

        fn view(&self) -> &WorldView {
            self.app.view.as_ref().expect("a view")
        }

        fn has_message(&self, text: &str) -> bool {
            self.app.messages.iter().any(|m| m.text.contains(text))
        }
    }

    #[test]
    fn keys_screens_and_overlay_work_end_to_end() {
        let mut h = Harness::new();
        h.step(vec![]);
        assert_eq!(h.app.screen, Screen::Open);
        assert_eq!(h.app.worlds.len(), 2);

        // Open a saved world: WorldOpened switches screens.
        let id = h.app.worlds[0].id.clone();
        h.intent(Intent::Submit(Action::OpenWorld(id)));
        assert_eq!(h.app.screen, Screen::Main);
        assert_eq!(h.view().children.len(), 3);
        h.step(vec![]);

        // Z zooms out; while busy, W is ignored, Escape opens help and
        // Escape again closes it.
        let root = h.view().current;
        h.key(Key::Z, Modifiers::NONE);
        assert!(h.app.engine.busy().is_some());
        h.key(Key::W, Modifiers::NONE);
        assert!(
            !h.has_message("already running"),
            "W should be dropped while busy, not submitted"
        );
        h.key(Key::Escape, Modifiers::NONE);
        assert!(h.app.help.open);
        h.step(vec![]);
        h.key(Key::Escape, Modifiers::NONE);
        assert!(!h.app.help.open);
        h.wait_idle();
        // The frame we left is outlined in the middle of the wider one.
        let back_link = h
            .view()
            .children
            .iter()
            .find(|c| c.node == root)
            .map(|c| c.transition.clone());
        assert!(matches!(back_link, Some(Transition::ZoomIn { .. })), "{back_link:?}");
        let zoomed_out = h.view().current;
        assert_eq!(h.view().depth, 1);

        // Backspace / Shift+Backspace: back and redo.
        h.key(Key::Backspace, Modifiers::NONE);
        assert_eq!(h.view().depth, 0);
        h.key(Key::Backspace, Modifiers::SHIFT);
        assert_eq!(h.view().current, zoomed_out);

        // Down pulls the nose up, toward the horizon; Up from straight down
        // is a no-op (already at the clamp).
        h.key(Key::ArrowUp, Modifiers::NONE);
        h.step(vec![]);
        assert!(h.app.engine.busy().is_none());
        h.key(Key::ArrowDown, Modifiers::NONE);
        h.wait_idle();
        assert_eq!(h.view().camera.tilt_deg, 30.0);

        // X cancels a running step.
        let before = h.view().current;
        h.key(Key::ArrowRight, Modifiers::NONE);
        assert!(h.app.engine.busy().is_some());
        h.key(Key::X, Modifiers::NONE);
        h.wait_idle();
        assert!(h.has_message("Cancelled"));
        assert_eq!(h.view().current, before);

        // X cancels even with the help overlay open.
        h.key(Key::ArrowLeft, Modifiers::NONE);
        h.key(Key::Escape, Modifiers::NONE);
        assert!(h.app.help.open && h.app.engine.busy().is_some());
        h.key(Key::X, Modifiers::NONE);
        h.wait_idle();
        assert_eq!(h.view().current, before);
        h.key(Key::Escape, Modifiers::NONE);
        assert!(!h.app.help.open);

        // R on a frame with branches asks first: Escape keeps everything.
        h.key(Key::Backspace, Modifiers::NONE);
        let (node, version) = (h.view().current, h.view().image_version);
        let branches = h.view().children.len();
        assert!(branches > 0);
        h.key(Key::R, Modifiers::NONE);
        assert_eq!(h.app.confirm_regenerate, Some(branches));
        assert!(h.app.engine.busy().is_none(), "nothing runs until confirmed");
        h.key(Key::W, Modifiers::NONE);
        assert!(h.app.engine.busy().is_none(), "other keys are swallowed by the modal");
        h.step(vec![]);
        h.key(Key::Escape, Modifiers::NONE);
        assert_eq!(h.app.confirm_regenerate, None);
        assert!(!h.app.help.open, "Escape closed the modal, not opened help");
        assert_eq!(h.view().image_version, version);
        assert_eq!(h.view().children.len(), branches);

        // R then Enter regenerates: same node, new image version, children discarded.
        h.key(Key::R, Modifiers::NONE);
        assert!(h.app.confirm_regenerate.is_some());
        h.step(vec![]);
        h.key(Key::Enter, Modifiers::NONE);
        assert_eq!(h.app.confirm_regenerate, None);
        assert!(h.app.engine.busy().is_some());
        h.wait_idle();
        assert_eq!(h.view().current, node);
        assert_eq!(h.view().image_version, version + 1);
        assert!(
            h.view().children.iter().all(|c| c.node == root || c.node == zoomed_out),
            "only back-links survive"
        );

        // A frame with no branches regenerates without asking.
        h.key(Key::ArrowDown, Modifiers::NONE);
        h.wait_idle();
        assert!(h.view().children.is_empty());
        let leaf_version = h.view().image_version;
        h.key(Key::R, Modifiers::NONE);
        assert_eq!(h.app.confirm_regenerate, None);
        assert!(h.app.engine.busy().is_some());
        h.wait_idle();
        assert_eq!(h.view().image_version, leaf_version + 1);

        // Back to the open screen; Lucky is busy there too and X cancels it.
        h.intent(Intent::GoToOpenScreen);
        assert_eq!(h.app.screen, Screen::Open);
        h.intent(Intent::Submit(Action::Lucky));
        assert!(h.app.engine.busy().is_some());
        h.key(Key::X, Modifiers::NONE);
        h.wait_idle();
        assert_eq!(h.app.screen, Screen::Open);

        // A photo opens at once (WorldOpened while still busy analysing);
        // keys other than Esc/X are ignored until the analysis finishes.
        let photo = std::env::temp_dir().join(format!("iz-app-photo-{}.png", std::process::id()));
        iz_types::imageops::save_png(
            &image::RgbaImage::from_pixel(60, 40, image::Rgba([200, 10, 10, 255])),
            &photo,
        )
        .unwrap();
        h.intent(Intent::Submit(Action::OpenPhoto(photo.clone())));
        for _ in 0..100 {
            if h.app.screen == Screen::Main {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            h.step(vec![]);
        }
        assert_eq!(h.app.screen, Screen::Main);
        assert!(h.app.engine.busy().is_some());
        h.key(Key::Z, Modifiers::NONE);
        assert!(!h.has_message("already running"));
        h.wait_idle();
        assert!(h.view().title.contains("analysed"), "{}", h.view().title);
        let _ = std::fs::remove_file(&photo);
        h.intent(Intent::GoToOpenScreen);

        // Lucky to completion opens the new world.
        h.intent(Intent::Submit(Action::Lucky));
        h.wait_idle();
        assert_eq!(h.app.screen, Screen::Main);
        assert_eq!(h.app.worlds.len(), 4);
        h.key(Key::Escape, Modifiers::NONE);
        h.step(vec![]);
        assert!(h.app.help.open);
    }
}
