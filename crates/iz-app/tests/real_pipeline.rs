//! End-to-end run against the real services. Costs money: three Claude
//! calls and three low-quality images. Runs only with `IZ_REAL_TESTS=1`.
//!
//! Lucky start → zoom in → zoom out, then checks the tree and leaves the
//! world folder behind for inspection (its path is printed).

use iz_types::{Action, Engine, EngineEvent, Quality, RectF, Settings, StdoutLog};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn run_action(engine: &mut dyn Engine, action: Action, settings: &Settings, timeout: Duration) -> Vec<EngineEvent> {
    let started = Instant::now();
    engine.submit(action.clone(), settings.clone()).expect("submit");
    let mut events = Vec::new();
    loop {
        events.extend(engine.poll());
        if engine.busy().is_none() {
            // The engine clears busy before its final events; one more drain.
            std::thread::sleep(Duration::from_millis(300));
            events.extend(engine.poll());
            break;
        }
        assert!(started.elapsed() < timeout, "{action:?} took longer than {timeout:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
    for e in &events {
        if let EngineEvent::Error(msg) = e {
            panic!("{action:?} failed: {msg}");
        }
    }
    println!("--- {action:?} finished in {:.0?}: {} events", started.elapsed(), events.len());
    events
}

#[test]
fn lucky_zoom_in_zoom_out() {
    if std::env::var(iz_types::env::REAL_TESTS).as_deref() != Ok("1") {
        eprintln!("skipped: set IZ_REAL_TESTS=1 to run");
        return;
    }
    iz_types::settings::load_dotenv();
    let data_dir = std::env::var("IZ_DATA_DIR").expect("set IZ_DATA_DIR to a scratch folder for this run");
    println!("data dir: {data_dir}");

    let settings = Settings { image_quality: Quality::Low, ..Settings::default() };
    let log: Arc<dyn iz_types::CallLog> = Arc::new(StdoutLog);
    let describer: Arc<dyn iz_types::Describer> = Arc::new(iz_claude::ClaudeCli::new());
    let painter: Arc<dyn iz_types::Painter> = Arc::new(iz_openai::OpenAiPainter::new());
    let mut engine = iz_world::RealEngine::new(describer, painter, log);
    let step_timeout = Duration::from_secs(600);

    run_action(&mut engine, Action::Lucky, &settings, step_timeout);
    let root = engine.view().expect("a world is open after lucky");
    println!("ROOT: {} · {} · {}", root.title, root.breadcrumb, root.image_path.display());
    assert_eq!(root.depth, 0);
    assert!(root.image_path.is_file());

    let rect = RectF::new(0.35, 0.35, 0.3, 0.3);
    run_action(&mut engine, Action::ZoomIn(rect), &settings, step_timeout);
    let zoomed = engine.view().expect("view after zoom in");
    println!("ZOOM IN: {} · {} · {}", zoomed.title, zoomed.breadcrumb, zoomed.image_path.display());
    assert_eq!(zoomed.depth, 1);
    assert_ne!(zoomed.current, root.current);
    assert!(zoomed.image_path.is_file());

    run_action(&mut engine, Action::ZoomOut, &settings, step_timeout);
    let out = engine.view().expect("view after zoom out");
    println!("ZOOM OUT: {} · {} · {}", out.title, out.breadcrumb, out.image_path.display());
    assert_eq!(out.depth, 2);
    assert!(out.image_path.is_file());
    let backlink = out.children.iter().find(|c| c.node == zoomed.current);
    assert!(backlink.is_some(), "the zoomed frame should appear as an outline on the zoom-out frame");
    println!("back-link outline: {:?}", backlink.unwrap().transition);

    // Re-entering through the outline must return the very same frame.
    run_action(&mut engine, Action::EnterChild(zoomed.current), &settings, Duration::from_secs(10));
    let again = engine.view().expect("view after enter child");
    assert_eq!(again.current, zoomed.current);
    assert_eq!(again.image_path, zoomed.image_path);
    println!("WORLD DIR: {}", root.image_path.parent().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default());
}
