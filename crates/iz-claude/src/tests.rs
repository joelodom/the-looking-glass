//! Crate-level tests: post-processing, the whole describe path against a
//! fake `claude` script (no network), and env-gated real calls.

use super::*;
use iz_types::{AncestorBrief, CallLog, Camera, RectF, Settings, StdoutLog};
use std::sync::Arc;
use std::time::Instant;

struct Collect(Mutex<Vec<LogEvent>>);

impl CallLog for Collect {
    fn event(&self, e: &LogEvent) {
        self.0.lock().unwrap().push(e.clone());
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("iz-claude-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn camera() -> Camera {
    Camera {
        frame_width_m: 400.0,
        distance_m: 350.0,
        tilt_deg: 0.0,
        heading_deg: 0.0,
        regime: "street".into(),
        scale_label: "about 400 m across".into(),
    }
}

fn prior() -> SceneSpec {
    SceneSpec {
        title: "Rooftops of Lisbon".into(),
        render_prompt: "An aerial photograph straight down over Alfama".into(),
        camera: camera(),
        canvas_mode: CanvasMode::Reference,
        dossier: json!({
            "location": "Alfama, Lisbon, Portugal",
            "established_facts": ["terracotta roofs", "a yellow tram on the left edge"]
        }),
        notes: String::new(),
    }
}

fn step(transition: Transition, world: &Path) -> DescribeRequest {
    DescribeRequest {
        kind: DescribeKind::Step,
        transition,
        current_image: Some(world.join("images/n0002.png")),
        crop_image: None,
        parent_image: None,
        prior_spec: Some(prior()),
        proposed_camera: Some(Camera {
            frame_width_m: 1600.0,
            distance_m: 1400.0,
            scale_label: String::new(),
            ..camera()
        }),
        ancestry: vec![AncestorBrief {
            title: "Lisbon".into(),
            transition: Transition::Root,
            camera: camera(),
        }],
        world_dir: world.to_path_buf(),
        settings: Settings::default(),
    }
}

// ---------------------------------------------------------------- finalize

#[test]
fn finalize_repairs_without_inventing() {
    let world = PathBuf::from("/w");
    let req = step(Transition::ZoomOut { factor: 4.0 }, &world);
    let mut spec = prior();
    spec.canvas_mode = CanvasMode::Crop;
    spec.camera.heading_deg = 370.0;
    spec.camera.frame_width_m = f64::NAN;
    spec.camera.scale_label = " ".into();
    spec.camera.regime = String::new();
    spec.dossier = json!({ "location": "Lisbon" });
    let out = finalize(spec, &req);
    assert_eq!(
        out.canvas_mode,
        CanvasMode::Reference,
        "only zoom_in may use crop"
    );
    assert_eq!(out.camera.heading_deg, 10.0);
    assert_eq!(
        out.camera.frame_width_m, 1600.0,
        "falls back to the proposal"
    );
    assert_eq!(out.camera.scale_label, "about 1.6 km across");
    assert_eq!(out.camera.regime, "street");
    assert_eq!(out.dossier["location"], "Lisbon");
    assert_eq!(
        out.dossier["established_facts"][1],
        "a yellow tram on the left edge"
    );
}

#[test]
fn finalize_keeps_crop_for_zoom_in_and_prior_dossier_when_missing() {
    let world = PathBuf::from("/w");
    let req = step(
        Transition::ZoomIn {
            rect: RectF::new(0.25, 0.25, 0.5, 0.5),
        },
        &world,
    );
    let mut spec = prior();
    spec.canvas_mode = CanvasMode::Crop;
    spec.dossier = Value::Null;
    let out = finalize(spec, &req);
    assert_eq!(out.canvas_mode, CanvasMode::Crop);
    assert_eq!(out.dossier, prior().dossier);
}

#[test]
fn claude_cli_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<ClaudeCli>();
    let _: Box<dyn Describer> = Box::new(ClaudeCli::new());
}

// ------------------------------------------------------- fake claude script

#[cfg(unix)]
mod fake {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const SCRIPT: &str = r#"#!/bin/sh
d="$(cd "$(dirname "$0")" && pwd)"
cat > "$d/stdin.txt"
env | cut -d= -f1 > "$d/env-names.txt"
pwd > "$d/cwd.txt"
for a in "$@"; do printf '%s\n' "$a"; done > "$d/args.txt"
if [ -f "$d/sleep" ]; then sleep 30; fi
cat "$d/reply.json"
if [ -f "$d/exit_code" ]; then exit "$(cat "$d/exit_code")"; fi
exit 0
"#;

    /// (bin dir with the fake `claude`, world dir, settings pointing at it)
    fn setup(name: &str, reply: &str) -> (PathBuf, PathBuf, Settings) {
        let root = temp_dir(name);
        let bin_dir = root.join("bin");
        let world = root.join("world");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("claude");
        std::fs::write(&bin, SCRIPT).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(bin_dir.join("reply.json"), reply).unwrap();
        let settings = Settings {
            claude_bin: Some(bin.display().to_string()),
            ..Settings::default()
        };
        (bin_dir, world, settings)
    }

    fn reply_with(spec: &Value) -> String {
        json!({
            "type": "result", "subtype": "success", "is_error": false, "num_turns": 2,
            "total_cost_usd": 0.05, "result": "", "structured_output": spec
        })
        .to_string()
    }

    #[test]
    fn round_trip_through_a_fake_cli() {
        let mut returned = serde_json::to_value(prior()).unwrap();
        returned["title"] = json!("Lisbon from 1.6 km");
        returned["canvas_mode"] = json!("crop");
        returned["camera"]["scale_label"] = json!("");
        returned["dossier"] = json!({ "location": "Lisbon, Portugal" });
        let (bin_dir, world, settings) = setup("roundtrip", &reply_with(&returned));
        let mut req = step(Transition::ZoomOut { factor: 4.0 }, &world);
        req.settings = settings;

        let log = Arc::new(Collect(Mutex::new(Vec::new())));
        let mut ctx = CallContext::new(log.clone());
        ctx.record_dir = Some(world.join("calls"));
        let cli = ClaudeCli::new();
        let spec = cli.describe(&req, &ctx).unwrap();

        assert_eq!(spec.title, "Lisbon from 1.6 km");
        assert_eq!(spec.canvas_mode, CanvasMode::Reference);
        assert_eq!(spec.camera.scale_label, "about 400 m across");
        assert_eq!(spec.dossier["established_facts"][0], "terracotta roofs");

        // The child saw the prompt on stdin, ran in the world dir, got the flags.
        let stdin = std::fs::read_to_string(bin_dir.join("stdin.txt")).unwrap();
        assert!(stdin.starts_with("REQUEST: step (zoom out ×4)"), "{stdin}");
        let cwd = std::fs::read_to_string(bin_dir.join("cwd.txt")).unwrap();
        assert_eq!(Path::new(cwd.trim()), world);
        let args = std::fs::read_to_string(bin_dir.join("args.txt")).unwrap();
        for flag in [
            "-p",
            "--json-schema",
            "--system-prompt",
            "--tools",
            "--allowedTools",
            "--max-turns",
        ] {
            assert!(args.lines().any(|l| l == flag), "missing {flag}");
        }
        assert!(args.contains("\"render_prompt\""), "schema passed inline");

        // Parent-session variables never reach the child.
        let names = std::fs::read_to_string(bin_dir.join("env-names.txt")).unwrap();
        let leaked: Vec<&str> = names
            .lines()
            .filter(|n| cli::is_leaked_session_var(n))
            .collect();
        assert!(leaked.is_empty(), "leaked into child: {leaked:?}");
        assert!(names
            .lines()
            .any(|n| n == "CLAUDE_CODE_DISABLE_ADVISOR_TOOL"));

        // Logs: request with the full system prompt, raw reply, summary note.
        let events = log.0.lock().unwrap().clone();
        let kinds: Vec<LogKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [LogKind::Request, LogKind::Response, LogKind::Note]);
        assert!(events
            .iter()
            .all(|e| e.service == "claude" && e.purpose == "describe zoom out"));
        assert!(events[0].text.contains(prompts::SYSTEM_PROMPT));
        assert!(events[0].text.contains("images/n0002.png"));
        assert!(events[1].text.contains("\"structured_output\""));
        assert!(events[2].text.contains("structured_output"));

        // A record per call.
        let records: Vec<_> = std::fs::read_dir(world.join("calls"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(records.len(), 1);
        let name = records[0].file_name().to_string_lossy().into_owned();
        assert!(
            name.starts_with("describe-zoom-out-") && name.ends_with(".json"),
            "{name}"
        );
        let rec: Value =
            serde_json::from_slice(&std::fs::read(records[0].path()).unwrap()).unwrap();
        assert_eq!(rec["user_prompt"], json!(stdin));
        assert_eq!(
            rec["reply"]["structured_output"]["title"],
            "Lisbon from 1.6 km"
        );

        // Second call: the system prompt is only referenced.
        log.0.lock().unwrap().clear();
        cli.describe(&req, &ctx).unwrap();
        let first = log.0.lock().unwrap()[0].text.clone();
        assert!(first.contains("system prompt: unchanged"));
        assert!(!first.contains(prompts::SYSTEM_PROMPT));
        let _ = std::fs::remove_dir_all(bin_dir.parent().unwrap());
    }

    #[test]
    fn rate_limited_cli_is_transient_and_recorded() {
        let reply = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 429 rate_limit_error"}"#;
        let (bin_dir, world, settings) = setup("ratelimit", reply);
        std::fs::write(bin_dir.join("exit_code"), "1").unwrap();
        let mut req = step(Transition::Forward, &world);
        req.settings = settings;
        let log = Arc::new(Collect(Mutex::new(Vec::new())));
        let mut ctx = CallContext::new(log.clone());
        ctx.record_dir = Some(world.join("calls"));
        let err = ClaudeCli::new().describe(&req, &ctx).unwrap_err();
        assert!(matches!(err, DescribeError::Transient(_)), "{err:?}");
        assert_eq!(log.0.lock().unwrap().last().unwrap().kind, LogKind::Error);
        assert_eq!(std::fs::read_dir(world.join("calls")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(bin_dir.parent().unwrap());
    }

    #[test]
    fn cancel_kills_the_child() {
        let (bin_dir, world, settings) = setup("cancel", "{}");
        std::fs::write(bin_dir.join("sleep"), "").unwrap();
        let mut req = step(
            Transition::Look {
                tilt_deg: 90.0,
                heading_deg: 45.0,
            },
            &world,
        );
        req.settings = settings;
        let ctx = CallContext::new(Arc::new(Collect(Mutex::new(Vec::new()))));
        let flag = ctx.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let err = ClaudeCli::new().describe(&req, &ctx).unwrap_err();
        assert!(matches!(err, DescribeError::Cancelled), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(bin_dir.parent().unwrap());
    }

    #[test]
    fn missing_binary_is_fatal() {
        let world = temp_dir("missing");
        let mut req = step(Transition::ZoomOut { factor: 4.0 }, &world);
        req.settings.claude_bin = Some(world.join("nope/claude").display().to_string());
        let ctx = CallContext::new(Arc::new(Collect(Mutex::new(Vec::new()))));
        let err = ClaudeCli::new().describe(&req, &ctx).unwrap_err();
        assert!(
            matches!(&err, DescribeError::Fatal(m) if m.contains("settings.claude_bin")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&world);
    }
}

// ------------------------------------------------------------- tiny PNGs

/// Minimal PNG writer (RGB8, stored deflate) so tests need no image crate.
fn write_png(path: &Path, w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 3]) {
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0); // filter: none
        for x in 0..w {
            raw.extend_from_slice(&px(x, y));
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (ty, data) in [(b"IHDR", &ihdr[..]), (b"IDAT", &z[..]), (b"IEND", &[][..])] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = ty.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, png).unwrap();
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Blue sky over green grass, with a yellow sun in the upper right.
fn landscape(x: u32, y: u32, w: u32, h: u32) -> [u8; 3] {
    let (sx, sy, r) = (w as i64 * 3 / 4, h as i64 / 5, h as i64 / 10);
    let (dx, dy) = (x as i64 - sx, y as i64 - sy);
    if dx * dx + dy * dy <= r * r {
        [250, 215, 40]
    } else if y < h / 2 {
        [70, 130, 220]
    } else {
        [50, 160, 60]
    }
}

#[test]
fn png_writer_is_valid() {
    let dir = temp_dir("png");
    let path = dir.join("t.png");
    write_png(&path, 300, 200, |x, y| landscape(x, y, 300, 200));
    let img = iz_types::imageops::load_rgba(&path).unwrap();
    assert_eq!((img.width(), img.height()), (300, 200));
    assert_eq!(img.get_pixel(10, 10).0, [70, 130, 220, 255]);
    assert_eq!(img.get_pixel(10, 190).0, [50, 160, 60, 255]);
    assert_eq!(img.get_pixel(225, 40).0, [250, 215, 40, 255]);
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------- real calls

fn real_tests() -> bool {
    std::env::var(iz_types::env::REAL_TESTS).as_deref() == Ok("1")
}

/// Real `claude` wrapped in a script that records the names of the
/// environment variables the child actually received.
#[cfg(unix)]
fn wrapped_real_claude(root: &Path) -> (Settings, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let real = ClaudeCli::new()
        .binary(&Settings::default())
        .expect("claude binary");
    println!("real claude: {}", real.display());
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let names = bin_dir.join("child-env-names.txt");
    let wrapper = bin_dir.join("claude");
    let script = format!(
        "#!/bin/sh\nenv | cut -d= -f1 > '{}'\nexec '{}' \"$@\"\n",
        names.display(),
        real.display()
    );
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    (
        Settings {
            claude_bin: Some(wrapper.display().to_string()),
            ..Settings::default()
        },
        names,
    )
}

#[cfg(unix)]
fn report_real_call(world: &Path, names: &Path, elapsed: Duration, spec: &SceneSpec) {
    println!(
        "\n=== SceneSpec ({:.1} s wall clock) ===",
        elapsed.as_secs_f64()
    );
    println!("{}", serde_json::to_string_pretty(spec).unwrap());

    let parent: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| cli::is_parent_session_var(k))
        .collect();
    let child = std::fs::read_to_string(names).unwrap();
    let session: Vec<&str> = child
        .lines()
        .filter(|n| cli::is_parent_session_var(n))
        .collect();
    println!("parent had {parent:?}; child received {session:?} (only CHILD_ENV is allowed)");
    assert!(!child.lines().any(cli::is_leaked_session_var));

    let record = std::fs::read_dir(world.join("calls"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let rec: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    let reply = &rec["reply"];
    let models: Vec<&String> = reply["modelUsage"]
        .as_object()
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    println!(
        "record {}: structured_output is {}, num_turns {}, permission_denials {}, cost ${}, duration_ms {}, models {models:?}",
        record.display(),
        if reply["structured_output"].is_object() { "an object" } else { "MISSING" },
        reply["num_turns"],
        reply["permission_denials"],
        reply["total_cost_usd"],
        reply["duration_ms"],
    );
}

/// `IZ_REAL_TESTS=1 cargo test -p iz-claude real_root -- --nocapture`
#[cfg(unix)]
#[test]
fn real_root_analysis() {
    if !real_tests() {
        return;
    }
    let root = temp_dir("real-root");
    let world = root.join("world");
    let photo = world.join("images/n0000.png");
    write_png(&photo, 384, 256, |x, y| landscape(x, y, 384, 256));
    let (settings, names) = wrapped_real_claude(&root);
    let req = DescribeRequest {
        kind: DescribeKind::Root,
        transition: Transition::Root,
        current_image: Some(photo),
        crop_image: None,
        parent_image: None,
        prior_spec: None,
        proposed_camera: None,
        ancestry: vec![],
        world_dir: world.clone(),
        settings,
    };
    let mut ctx = CallContext::new(Arc::new(StdoutLog));
    ctx.record_dir = Some(world.join("calls"));
    let started = Instant::now();
    let spec = ClaudeCli::new()
        .describe(&req, &ctx)
        .expect("root analysis");
    report_real_call(&world, &names, started.elapsed(), &spec);
    let text = serde_json::to_string(&spec).unwrap().to_lowercase();
    assert!(
        ["blue", "green", "sky", "grass", "sun", "yellow"]
            .iter()
            .any(|w| text.contains(w)),
        "the reply shows no sign of having seen the image"
    );
}

/// `IZ_REAL_TESTS=1 cargo test -p iz-claude real_lucky -- --nocapture`
#[cfg(unix)]
#[test]
fn real_lucky() {
    if !real_tests() {
        return;
    }
    let root = temp_dir("real-lucky");
    let world = root.join("world");
    let (settings, names) = wrapped_real_claude(&root);
    let req = DescribeRequest {
        kind: DescribeKind::Lucky,
        transition: Transition::Root,
        current_image: None,
        crop_image: None,
        parent_image: None,
        prior_spec: None,
        proposed_camera: None,
        ancestry: vec![],
        world_dir: world.clone(),
        settings,
    };
    let mut ctx = CallContext::new(Arc::new(StdoutLog));
    ctx.record_dir = Some(world.join("calls"));
    let started = Instant::now();
    let spec = ClaudeCli::new().describe(&req, &ctx).expect("lucky");
    report_real_call(&world, &names, started.elapsed(), &spec);
    assert!(
        spec.render_prompt.len() > 200,
        "lucky needs a full render prompt"
    );
}

/// `IZ_REAL_TESTS=1 cargo test -p iz-claude real_step -- --nocapture`
/// A 4x zoom into the sun of the sky-over-grass picture: two images to Read.
#[cfg(unix)]
#[test]
fn real_step_zoom_in() {
    if !real_tests() {
        return;
    }
    let root = temp_dir("real-step");
    let world = root.join("world");
    let current = world.join("images/n0000.png");
    let crop = world.join("tmp/crop.png");
    write_png(&current, 384, 256, |x, y| landscape(x, y, 384, 256));
    // The upscaled crop around the sun: mostly the yellow disc on blue.
    write_png(&crop, 384, 256, |x, y| {
        let (dx, dy) = (x as i64 - 192, y as i64 - 128);
        if dx * dx + dy * dy <= 100 * 100 {
            [250, 215, 40]
        } else {
            [70, 130, 220]
        }
    });
    let (settings, names) = wrapped_real_claude(&root);
    let prior = SceneSpec {
        title: "Meadow under a summer sky".into(),
        render_prompt: "A photograph of a flat green meadow under a clear blue sky with the sun high on the right"
            .into(),
        camera: Camera {
            frame_width_m: 2000.0,
            distance_m: 1500.0,
            tilt_deg: 88.0,
            heading_deg: 180.0,
            regime: "geographic".into(),
            scale_label: "about 2 km across".into(),
        },
        canvas_mode: CanvasMode::Reference,
        dossier: json!({
            "location": "an unidentified lowland meadow, temperate climate",
            "time_of_day": "midday", "season": "summer", "weather": "clear",
            "world_depth": 0, "world_seed": null,
            "established_facts": ["flat meadow fills the lower half", "the sun is in the upper right"]
        }),
        notes: String::new(),
    };
    let rect = RectF::new(0.625, 0.075, 0.25, 0.25);
    let req = DescribeRequest {
        kind: DescribeKind::Step,
        transition: Transition::ZoomIn { rect },
        current_image: Some(current),
        crop_image: Some(crop),
        parent_image: None,
        proposed_camera: Some(Camera {
            frame_width_m: 500.0,
            distance_m: 375.0,
            scale_label: String::new(),
            ..prior.camera.clone()
        }),
        prior_spec: Some(prior.clone()),
        ancestry: vec![],
        world_dir: world.clone(),
        settings,
    };
    let mut ctx = CallContext::new(Arc::new(StdoutLog));
    ctx.record_dir = Some(world.join("calls"));
    let started = Instant::now();
    let spec = ClaudeCli::new().describe(&req, &ctx).expect("zoom in");
    report_real_call(&world, &names, started.elapsed(), &spec);
    let facts = spec.dossier["established_facts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!("established_facts now: {facts:?}");
    assert!(facts.len() >= 2, "facts must not be dropped");
}
