//! End-to-end runs of `RealEngine` over the mock services, with
//! `IZ_DATA_DIR` pointed at a temp dir.

use image::{Rgba, RgbaImage};
use iz_types::log::{CallLog, LogEvent, LogKind};
use iz_types::{
    imageops, Action, Backlink, Engine, EngineError, EngineEvent, Outcome, RectF, Service, Settings, StatusEvent,
    Transition, World, WorldView,
};
use iz_world::{store, MockDescriber, MockPainter, RealEngine};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Set `IZ_DATA_DIR` once for this test binary, before any path is resolved.
fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("iz-world-e2e-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(iz_types::env::DATA_DIR, &dir);
        dir
    })
}

#[derive(Default)]
struct MemLog(Mutex<Vec<LogEvent>>);

impl CallLog for MemLog {
    fn event(&self, event: &LogEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

fn settings() -> Settings {
    Settings { frame_width: 384, frame_height: 256, zoom_out_factor: 4.0, ..Settings::default() }
}

struct Harness {
    engine: RealEngine,
    painter: Arc<MockPainter>,
    log: Arc<MemLog>,
}

fn harness(describe_ms: u64, paint_ms: u64) -> Harness {
    data_dir();
    let painter = Arc::new(MockPainter::with_delay(Duration::from_millis(paint_ms)));
    let log = Arc::new(MemLog::default());
    let engine = RealEngine::with_services(
        Arc::new(MockDescriber::with_delay(Duration::from_millis(describe_ms))),
        painter.clone(),
        log.clone(),
        Service::Mock,
        Service::Mock,
    );
    Harness { engine, painter, log }
}

impl Harness {
    /// Submit and wait until the action is over; returns its events.
    fn run(&mut self, action: Action) -> Vec<EngineEvent> {
        self.engine.submit(action, settings()).expect("submit");
        self.engine.wait_idle();
        assert!(!self.engine.is_running() && self.engine.busy().is_none());
        self.engine.poll()
    }

    fn view(&self) -> WorldView {
        self.engine.view().expect("a world is open")
    }

    fn world(&self) -> World {
        self.engine.world_snapshot().expect("a world is open")
    }
}

fn write_photo(name: &str) -> PathBuf {
    let path = data_dir().join(name);
    let img = RgbaImage::from_fn(600, 400, |x, y| Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]));
    img.save(&path).unwrap();
    path
}

fn finished(events: &[EngineEvent]) -> Vec<(String, Outcome)> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Status(StatusEvent::Finished { purpose, outcome, service, .. }) => {
                assert_eq!(*service, Service::Mock);
                Some((purpose.clone(), outcome.clone()))
            }
            _ => None,
        })
        .collect()
}

fn ok(purposes: &[&str]) -> Vec<(String, Outcome)> {
    purposes.iter().map(|p| (p.to_string(), Outcome::Ok)).collect()
}

fn infos(events: &[EngineEvent]) -> Vec<String> {
    events.iter().filter_map(|e| if let EngineEvent::Info(s) = e { Some(s.clone()) } else { None }).collect()
}

fn errors(events: &[EngineEvent]) -> Vec<String> {
    events.iter().filter_map(|e| if let EngineEvent::Error(s) = e { Some(s.clone()) } else { None }).collect()
}

fn count(events: &[EngineEvent], wanted: &EngineEvent) -> usize {
    events.iter().filter(|e| *e == wanted).count()
}

fn dims(path: &Path) -> (u32, u32) {
    imageops::load_rgba(path).unwrap().dimensions()
}

#[test]
fn mock_end_to_end() {
    let mut h = harness(20, 20);
    let photo = write_photo("street.png");

    // Open a photo: shown first, then analysed.
    let ev = h.run(Action::OpenPhoto(photo));
    assert_eq!(errors(&ev), Vec::<String>::new());
    assert_eq!(finished(&ev), ok(&["describe root"]));
    assert_eq!(count(&ev, &EngineEvent::WorldOpened), 1);
    assert_eq!(count(&ev, &EngineEvent::ViewChanged), 2);
    assert!(count(&ev, &EngineEvent::WorldsChanged) >= 1);
    let opened = ev.iter().position(|e| *e == EngineEvent::WorldOpened).unwrap();
    let started = ev.iter().position(|e| matches!(e, EngineEvent::Status(StatusEvent::Started { .. }))).unwrap();
    assert!(opened < started, "the photo is shown before the analysis starts");
    let v = h.view();
    let (world_id, root) = (v.world_id.clone(), v.current);
    assert!(world_id[..12].chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!((v.depth, v.can_back, v.can_redo), (0, false, false));
    assert_eq!(v.title, "A street in an unidentified town");
    assert_eq!(v.world_title, v.title);
    assert_eq!(v.breadcrumb, "street · about 30 m across · depth 0");
    let dir = store::world_dir(&world_id);
    for file in ["world.json", "images/n0001.png", "thumb.png", "log.txt"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    assert!(dir.join("calls").is_dir());
    assert_eq!(dims(&v.image_path), (600, 400));
    assert_eq!(h.world().source_photo.as_deref().and_then(Path::file_name), Some("street.png".as_ref()));

    // Zoom in.
    let ev = h.run(Action::ZoomIn(RectF::new(0.25, 0.25, 0.5, 0.333)));
    assert_eq!(finished(&ev), ok(&["describe zoom in", "paint zoom in"]));
    assert_eq!(count(&ev, &EngineEvent::ViewChanged), 1);
    let v = h.view();
    let zoomed = v.current;
    let zoomed_image = v.image_path.clone();
    assert_ne!(zoomed, root);
    assert_eq!((v.depth, v.can_back), (1, true));
    assert!((v.camera.frame_width_m - 15.0).abs() < 1e-3, "{}", v.camera.frame_width_m);
    assert_eq!(dims(&zoomed_image), (384, 256));
    assert_eq!(dims(&dir.join("tmp/crop.png")), (384, 256));

    // Zoom out: the original frame becomes a centered zoom-in outline.
    let ev = h.run(Action::ZoomOut);
    assert_eq!(finished(&ev), ok(&["describe zoom out", "paint zoom out"]));
    let v = h.view();
    let z = v.current;
    assert_eq!(v.depth, 2);
    assert!((v.camera.frame_width_m - 60.0).abs() < 1e-3);
    let outline = v.children.iter().find(|c| c.node == zoomed).expect("back-link outline on the zoom-out frame");
    let Transition::ZoomIn { rect } = outline.transition else { panic!("{:?}", outline.transition) };
    assert!(rect.approx_eq(&RectF::centered(0.25, 0.25), 0.01), "{rect:?}");
    let w = h.world();
    assert_eq!(w.nodes[&zoomed].backlink, Some(Backlink { zoom_out_node: z, rect }));
    assert_eq!(w.nodes[&z].parent, Some(zoomed));
    assert_eq!(w.find_child(z, &Transition::ZoomIn { rect }), Some(zoomed));

    // Entering the outline returns to the very same frame.
    let ev = h.run(Action::EnterChild(zoomed));
    assert!(finished(&ev).is_empty());
    assert_eq!(count(&ev, &EngineEvent::ViewChanged), 1);
    assert_eq!(h.view().current, zoomed);
    assert_eq!(h.view().image_path, zoomed_image);

    // The same zoom out again is reused, not regenerated.
    let ev = h.run(Action::ZoomOut);
    assert!(finished(&ev).is_empty());
    assert!(infos(&ev).iter().any(|i| i.contains("cached")), "{ev:?}");
    assert_eq!(h.view().current, z);
    assert_eq!(h.world().nodes.len(), 3);
    h.run(Action::EnterChild(zoomed));

    // Tilt up to the horizon; further up is a no-op.
    assert_eq!(h.view().camera.tilt_deg, 60.0);
    let ev = h.run(Action::Tilt { delta_deg: 30.0 });
    assert_eq!(finished(&ev), ok(&["describe look", "paint look"]));
    let look = h.view().current;
    assert_eq!(h.view().camera.tilt_deg, 90.0);
    assert_eq!(h.world().nodes[&look].transition, Transition::Look { tilt_deg: 90.0, heading_deg: 0.0 });
    let ev = h.run(Action::Tilt { delta_deg: 30.0 });
    assert!(finished(&ev).is_empty());
    assert_eq!(infos(&ev), vec!["already looking that way".to_string()]);
    assert_eq!(h.view().current, look);

    // Back and redo.
    h.run(Action::Back);
    assert_eq!(h.view().current, zoomed);
    assert!(h.view().can_redo);
    h.run(Action::Redo);
    assert_eq!(h.view().current, look);
    assert!(!h.view().can_redo);

    // Regenerate a leaf: same node, new image version.
    let ev = h.run(Action::Regenerate);
    assert_eq!(finished(&ev), ok(&["describe look", "paint look"]));
    assert_eq!(h.view().current, look);
    assert_eq!(h.view().image_version, 1);
    assert_eq!(h.world().nodes.len(), 4);

    // Regenerate the zoom-out node: its back-link child (its own parent) stays.
    h.run(Action::Back);
    h.run(Action::EnterChild(z));
    let ev = h.run(Action::Regenerate);
    assert_eq!(finished(&ev), ok(&["describe zoom out", "paint zoom out"]));
    let w = h.world();
    assert_eq!(w.nodes.len(), 4);
    assert!(w.nodes[&z].children.contains(&zoomed));
    assert_eq!(w.nodes[&zoomed].backlink.as_ref().map(|b| b.zoom_out_node), Some(z));
    assert!(h.view().children.iter().any(|c| c.node == zoomed));

    // Regenerate the zoomed frame: its descendants go, their files stay.
    h.run(Action::EnterChild(zoomed));
    let ev = h.run(Action::Regenerate);
    assert_eq!(finished(&ev), ok(&["describe zoom in", "paint zoom in"]));
    assert!(infos(&ev).iter().any(|i| i.contains("discarded")), "{ev:?}");
    let w = h.world();
    assert_eq!(w.nodes.keys().copied().collect::<Vec<_>>(), vec![root, zoomed]);
    assert_eq!(w.nodes[&zoomed].backlink, None);
    assert!(h.view().children.is_empty());
    assert_eq!(h.view().image_version, 1);
    for n in [z, look] {
        assert!(store::abs_path(&world_id, &store::image_rel_path(n)).is_file());
    }

    // world.json reloads to the same tree, and a fresh engine reopens it
    // where we left off.
    assert_eq!(store::load(&world_id).unwrap(), h.world());
    let mut h2 = harness(20, 20);
    let ev = h2.run(Action::OpenWorld(world_id.clone()));
    assert_eq!(ev, vec![EngineEvent::WorldOpened, EngineEvent::ViewChanged]);
    assert_eq!(h2.view(), h.view());
    let listed = h2.engine.list_worlds().into_iter().find(|s| s.id == world_id).expect("listed");
    assert_eq!(listed.node_count, 2);
    assert!(listed.thumbnail.is_some_and(|t| t.is_file()));

    // Prompts and replies were logged like the real services, also to log.txt.
    let log = h.log.0.lock().unwrap();
    for (purpose, kind) in [("describe root", LogKind::Request), ("paint zoom out", LogKind::Response)] {
        assert!(log.iter().any(|e| e.service == "mock" && e.purpose == purpose && e.kind == kind), "{purpose}");
    }
    let text = std::fs::read_to_string(dir.join("log.txt")).unwrap();
    assert!(text.contains("describe zoom in") && text.contains("paint look"));
}

#[test]
fn cancel_leaves_the_world_unchanged() {
    let mut h = harness(600, 600);
    h.run(Action::OpenPhoto(write_photo("cancel.png")));
    let before = h.world();

    for (wait_ms, purpose, step) in [(200, "describe zoom in", "1/2 describe"), (850, "paint zoom in", "2/2 paint")] {
        h.engine.submit(Action::ZoomIn(RectF::new(0.1, 0.1, 0.3, 0.3)), settings()).unwrap();
        assert!(matches!(h.engine.submit(Action::ZoomOut, settings()), Err(EngineError::Busy)));
        std::thread::sleep(Duration::from_millis(wait_ms));
        let busy = h.engine.busy().expect("busy during the call");
        assert_eq!((busy.service, busy.purpose.as_str(), busy.step.as_str()), (Service::Mock, purpose, step));

        h.engine.cancel();
        let t = Instant::now();
        h.engine.wait_idle();
        assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
        let ev = h.engine.poll();
        assert_eq!(finished(&ev).last(), Some(&(purpose.to_string(), Outcome::Cancelled)));
        assert_eq!(infos(&ev), vec!["cancelled".to_string()]);
        assert_eq!(count(&ev, &EngineEvent::ViewChanged), 0);
        assert!(h.engine.busy().is_none() && !h.engine.is_running());
        assert_eq!(h.world(), before);
        assert_eq!(store::load(&before.id).unwrap(), before);
    }

    // The engine takes the next action.
    let ev = h.run(Action::Back);
    assert_eq!(infos(&ev), vec!["Already at the start.".to_string()]);
}

#[test]
fn a_refusal_is_rewritten_once_then_given_up() {
    let mut h = harness(5, 5);
    h.run(Action::OpenPhoto(write_photo("refusal.png")));

    h.painter.refuse_next(1);
    let ev = h.run(Action::ZoomIn(RectF::new(0.2, 0.2, 0.25, 0.25)));
    assert_eq!(
        finished(&ev),
        vec![
            ("describe zoom in".to_string(), Outcome::Ok),
            ("paint zoom in".to_string(), Outcome::Refused),
            ("rewrite zoom in".to_string(), Outcome::Ok),
            ("paint zoom in".to_string(), Outcome::Ok),
        ]
    );
    assert!(errors(&ev).is_empty());
    let w = h.world();
    let spec = w.nodes[&w.current].spec.as_ref().unwrap();
    assert!(spec.render_prompt.starts_with("A generic, safe"));
    assert_eq!(h.view().depth, 1);

    h.painter.refuse_next(2);
    let before = h.world();
    let ev = h.run(Action::ZoomOut);
    assert_eq!(finished(&ev).len(), 4);
    assert_eq!(errors(&ev), vec!["Can't go further here.".to_string()]);
    assert_eq!(h.world(), before);
}

#[test]
fn feeling_lucky_paints_a_new_world() {
    let mut h = harness(5, 5);
    assert!(matches!(h.engine.submit(Action::ZoomOut, settings()), Err(EngineError::NoWorld)));
    assert!(h.engine.view().is_none());

    let ev = h.run(Action::Lucky);
    assert_eq!(finished(&ev), ok(&["describe lucky", "paint lucky"]));
    for e in [EngineEvent::WorldOpened, EngineEvent::ViewChanged, EngineEvent::WorldsChanged] {
        assert_eq!(count(&ev, &e), 1, "{e:?}");
    }
    let v = h.view();
    assert_eq!(v.depth, 0);
    assert!(v.title.ends_with("from the air"), "{}", v.title);
    assert_eq!(dims(&v.image_path), (384, 256));
    let w = h.world();
    assert_eq!(w.source_photo, None);
    assert!(store::thumb_path(&w.id).is_file());

    // Regenerating a lucky start repaints it.
    let ev = h.run(Action::Regenerate);
    assert_eq!(finished(&ev), ok(&["describe lucky", "paint lucky"]));
    assert_eq!(h.view().image_version, 1);
    assert_eq!(h.world().nodes.len(), 1);
}

/// A describer that always fails, to pin the "analysis failed" semantics.
struct FailingDescriber;

impl iz_types::Describer for FailingDescriber {
    fn describe(
        &self,
        _req: &iz_types::DescribeRequest,
        _ctx: &iz_types::CallContext,
    ) -> Result<iz_types::SceneSpec, iz_types::DescribeError> {
        Err(iz_types::DescribeError::Transient("no network".into()))
    }
}

#[test]
fn a_failed_photo_analysis_keeps_the_world() {
    data_dir();
    let mut engine = RealEngine::new(
        Arc::new(FailingDescriber),
        Arc::new(MockPainter::with_delay(Duration::ZERO)),
        Arc::new(MemLog::default()),
    );
    engine.submit(Action::OpenPhoto(write_photo("offline.png")), settings()).unwrap();
    engine.wait_idle();
    let ev = engine.poll();
    assert_eq!(count(&ev, &EngineEvent::WorldOpened), 1);
    let failed = finished_any(&ev);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].0, Service::Claude);
    assert!(matches!(&failed[0].1, Outcome::Failed(m) if m.contains("no network")));
    assert_eq!(errors(&ev).len(), 1, "{ev:?}");
    let view = engine.view().expect("the world stays open");
    assert_eq!(view.title, "offline");
    assert_eq!(view.breadcrumb, "start · depth 0");
    let world = engine.world_snapshot().unwrap();
    assert_eq!(world.nodes[&world.root].spec, None);
    assert_eq!(store::load(&world.id).unwrap(), world);
}

fn finished_any(events: &[EngineEvent]) -> Vec<(Service, Outcome)> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Status(StatusEvent::Finished { service, outcome, .. }) => Some((*service, outcome.clone())),
            _ => None,
        })
        .collect()
}

/// The production constructor at the default 1536x1024 frame size, with the
/// default mock delays (about 7 s). Run with `cargo test -- --ignored`.
#[test]
#[ignore = "slow: default mock delays"]
fn mock_engine_at_default_frame_size() {
    data_dir();
    let mut engine = iz_world::mock_engine(Arc::new(iz_types::StdoutLog));
    let settings = Settings::default();
    for action in [
        Action::OpenPhoto(write_photo("default-size.png")),
        Action::ZoomIn(RectF::new(0.1, 0.1, 0.3, 0.3)),
        Action::ZoomOut,
    ] {
        engine.submit(action, settings.clone()).unwrap();
        engine.wait_idle();
        let ev = engine.poll();
        assert!(errors(&ev).is_empty(), "{ev:?}");
    }
    let view = engine.view().unwrap();
    assert_eq!(view.depth, 2);
    assert_eq!(dims(&view.image_path), (1536, 1024));
    assert!(view.children.iter().any(|c| matches!(c.transition, Transition::ZoomIn { .. })));
}
