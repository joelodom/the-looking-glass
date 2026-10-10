//! Painter tests without a network: a tiny HTTP server on 127.0.0.1 plays
//! the API.

use super::*;
use crate::plan::tests::request;
use image::Rgba;
use iz_types::{CallLog, Quality};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Debug, Clone)]
struct Captured {
    path: String,
    body: String,
}

impl Captured {
    /// A text field from a JSON or multipart body.
    fn field(&self, name: &str) -> Option<String> {
        if let Ok(v) = serde_json::from_str::<Value>(&self.body) {
            return v.get(name).map(|f| f.as_str().map_or(f.to_string(), str::to_string));
        }
        let marker = format!("name=\"{name}\"\r\n\r\n");
        let rest = &self.body[self.body.find(&marker)? + marker.len()..];
        Some(rest[..rest.find("\r\n")?].to_string())
    }

    fn model(&self) -> String {
        self.field("model").unwrap_or_default()
    }

    fn count(&self, needle: &str) -> usize {
        self.body.matches(needle).count()
    }
}

struct Reply {
    status: u16,
    body: String,
    delay: Duration,
}

fn reply(status: u16, body: impl Into<String>) -> Reply {
    Reply { status, body: body.into(), delay: Duration::ZERO }
}

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Captured>>>,
}

impl Mock {
    fn seen(&self) -> Vec<Captured> {
        lock(&self.seen).clone()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn read_request(s: &mut TcpStream) -> Option<Captured> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 64 * 1024];
    let header_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
    let length = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
    });
    let chunked = head.to_ascii_lowercase().contains("transfer-encoding: chunked");
    let done = |buf: &Vec<u8>| match length {
        Some(len) => buf.len() >= header_end + len,
        None => !chunked || buf.ends_with(b"0\r\n\r\n"),
    };
    while !done(&buf) {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(Captured { path, body: String::from_utf8_lossy(&buf[header_end..]).to_string() })
}

fn serve(responder: impl Fn(&Captured) -> Reply + Send + Sync + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let responder = Arc::new(responder);
    let seen_srv = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let responder = Arc::clone(&responder);
            let seen = Arc::clone(&seen_srv);
            std::thread::spawn(move || {
                let Some(req) = read_request(&mut stream) else { return };
                let r = responder(&req);
                lock(&seen).push(req);
                std::thread::sleep(r.delay);
                let head = format!(
                    "HTTP/1.1 {} Mock\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    r.status,
                    r.body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(r.body.as_bytes());
            });
        }
    });
    Mock { base, seen }
}

fn ok_body(w: u32, h: u32) -> String {
    let png = imageops::encode_png(&RgbaImage::from_pixel(w, h, Rgba([200, 10, 10, 255]))).unwrap();
    json!({
        "created": 1,
        "data": [{"b64_json": BASE64_STANDARD.encode(png)}],
        "usage": {
            "input_tokens": 50,
            "input_tokens_details": {"text_tokens": 50, "image_tokens": 0},
            "output_tokens": 272,
            "total_tokens": 322
        }
    })
    .to_string()
}

const MODEL_404: &str = r#"{"error":{"message":"The model `x` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#;
const SERVER_500: &str = r#"{"error":{"message":"The server had an error while processing your request.","type":"server_error","param":null,"code":null}}"#;

#[derive(Default)]
struct CaptureLog(Mutex<Vec<LogEvent>>);

impl CallLog for CaptureLog {
    fn event(&self, event: &LogEvent) {
        lock(&self.0).push(event.clone());
    }
}

impl CaptureLog {
    fn of_kind(&self, kind: LogKind) -> Vec<String> {
        lock(&self.0).iter().filter(|e| e.kind == kind).map(|e| e.text.clone()).collect()
    }
}

struct Harness {
    painter: OpenAiPainter,
    ctx: CallContext,
    log: Arc<CaptureLog>,
    dir: PathBuf,
}

impl Harness {
    fn new(mock: &Mock, name: &str) -> Harness {
        let dir = std::env::temp_dir().join(format!("iz-openai-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut painter = OpenAiPainter::new()
            .with_base_url(&mock.base)
            .with_api_key("sk-test")
            .with_ledger_path(dir.join("ledger.jsonl"));
        painter.retry_base = Duration::from_millis(5);
        let log = Arc::new(CaptureLog::default());
        let ctx = CallContext::new(log.clone());
        Harness { painter, ctx, log, dir }
    }

    fn ledger(&self) -> Vec<Value> {
        read_ledger(&self.dir.join("ledger.jsonl"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_ledger(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("ledger line is JSON"))
        .collect()
}

#[test]
fn painter_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OpenAiPainter>();
}

#[test]
fn key_resolution() {
    assert_eq!(resolve_key(Some("sk-a"), Some("sk-b")).unwrap(), "sk-a");
    assert_eq!(resolve_key(None, Some(" sk-b\n")).unwrap(), "sk-b");
    let err = resolve_key(None, None).unwrap_err();
    assert!(matches!(&err, PaintError::Fatal(m) if m.contains("OPENAI_API_KEY")), "{err}");
    assert!(resolve_key(None, Some("  ")).is_err());
}

#[test]
fn backoff_grows_and_honors_retry_after() {
    let base = Duration::from_millis(1000);
    for retry in 0..3 {
        let d = backoff_delay(base, retry, None);
        let lo = base * 2u32.pow(retry);
        assert!(d >= lo && d <= lo + base, "retry {retry}: {d:?}");
    }
    assert!(backoff_delay(base, 0, Some(Duration::from_secs(7))) >= Duration::from_secs(7));
    assert_eq!(backoff_delay(base, 0, Some(Duration::from_secs(600))), MAX_BACKOFF);
}

#[test]
fn file_stems() {
    assert_eq!(file_stem("paint zoom out"), "paint-zoom-out");
    assert_eq!(file_stem(" Lucky / Root! "), "lucky-root");
    assert_eq!(file_stem(""), "paint");
}

#[test]
fn validation() {
    let mut r = request(false, true, 0);
    assert!(matches!(validate(&r), Err(PaintError::Fatal(m)) if m.contains("without a canvas")));
    r.canvas = Some(RgbaImage::new(100, 100));
    assert!(matches!(validate(&r), Err(PaintError::Fatal(m)) if m.contains("must match")));
    let mut r = request(false, false, 0);
    r.prompt = "  ".into();
    assert!(validate(&r).is_err());
    assert!(validate(&request(true, true, 2)).is_ok());
}

#[test]
fn inputs_keep_canvas_alpha_and_make_references_opaque() {
    let mut r = request(true, true, 1);
    r.canvas = Some(RgbaImage::from_pixel(1536, 1024, Rgba([1, 2, 3, 0])));
    r.mask = Some(RgbaImage::from_pixel(1536, 1024, Rgba([0, 0, 0, 0])));
    r.references = vec![RgbaImage::from_pixel(640, 480, Rgba([9, 9, 9, 10]))];
    let (inputs, notes) = encode_inputs(&r, (1536, 1024)).unwrap();
    assert!(notes.is_empty());
    assert_eq!(inputs.images.len(), 2);
    assert_eq!(inputs.images[0].0, "canvas.png");
    let canvas = imageops::decode(&inputs.images[0].1).unwrap();
    assert_eq!(canvas.get_pixel(0, 0)[3], 0);
    let reference = imageops::decode(&inputs.images[1].1).unwrap();
    assert_eq!(reference.dimensions(), (640, 480));
    assert_eq!(reference.get_pixel(0, 0)[3], 255);
    let mask = imageops::decode(inputs.mask.as_ref().unwrap()).unwrap();
    assert_eq!(mask.get_pixel(5, 5)[3], 0);

    // A standard-size fallback resizes canvas and mask together.
    let (inputs, notes) = encode_inputs(&r, (1024, 1024)).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(imageops::decode(&inputs.images[0].1).unwrap().dimensions(), (1024, 1024));
    assert_eq!(imageops::decode(inputs.mask.as_ref().unwrap()).unwrap().dimensions(), (1024, 1024));

    // At most 16 images go to the API.
    let r = request(true, false, 20);
    let (inputs, notes) = encode_inputs(&r, (1536, 1024)).unwrap();
    assert_eq!(inputs.images.len(), 16);
    assert_eq!(notes.len(), 1);
}

#[test]
fn interpret_replies() {
    let raw = |status, body: &str| Ok(RawResponse { status, retry_after: None, body: body.into() });
    let it = interpret(raw(200, &ok_body(16, 16)));
    assert!(it.outcome.is_ok());
    assert_eq!(it.usage.as_ref().unwrap()["output_tokens"], 272);
    let it = interpret(raw(200, r#"{"data":[]}"#));
    assert!(matches!(&it.outcome, Err(f) if f.class == ErrorClass::Fatal && f.message.contains("no image")));
    let it = interpret(raw(429, r#"{"error":{"message":"slow down","code":"rate_limit_exceeded"}}"#));
    assert!(matches!(&it.outcome, Err(f) if f.class == ErrorClass::Transient));
}

#[test]
fn generation_end_to_end() {
    let mock = serve(|_| reply(200, ok_body(1536, 1024)));
    let mut h = Harness::new(&mock, "gen");
    h.ctx.record_dir = Some(h.dir.join("calls"));
    let req = request(false, false, 0);
    let r = h.painter.paint(&req, &h.ctx).expect("paint");
    assert_eq!(r.image.dimensions(), (1536, 1024));
    assert_eq!(r.model_used, "gpt-image-2.5-sunburst");
    assert_eq!(r.usage.as_ref().unwrap()["output_tokens"], 272);
    // 50 text tokens at $5/M + 272 image tokens at $30/M.
    assert!((r.estimated_usd.unwrap() - 0.00841).abs() < 1e-9);

    let seen = mock.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/images/generations");
    let body: Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(body["size"], "1536x1024");
    assert_eq!(body["quality"], "high");
    assert_eq!(body["output_format"], "png");
    assert_eq!(body["n"], 1);

    let ledger = h.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0]["outcome"], "ok");
    assert_eq!(ledger[0]["endpoint"], "generations");
    assert_eq!(ledger[0]["attempt"], 1);

    let requests = h.log.of_kind(LogKind::Request);
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("prompt:\na lighthouse"));
    let responses = h.log.of_kind(LogKind::Response);
    assert!(responses[0].contains("model used: gpt-image-2.5-sunburst"), "{}", responses[0]);

    let files: Vec<String> = std::fs::read_dir(h.dir.join("calls"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    let json_file = files.iter().find(|f| f.ends_with(".json")).unwrap();
    assert!(json_file.starts_with("test-"));
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(h.dir.join("calls").join(json_file)).unwrap()).unwrap();
    assert_eq!(record["outcome"], "ok");
    assert_eq!(record["prompt"], "a lighthouse");
    assert!(record["image_file"].as_str().unwrap().ends_with(".png"));
    assert!(!record.to_string().contains("b64_json"));
}

#[test]
fn edits_send_images_mask_and_fields() {
    let mock = serve(|_| reply(200, ok_body(1536, 1024)));
    let h = Harness::new(&mock, "edits");
    let mut req = request(true, true, 2);
    req.model = "gpt-image-1.5".into();
    h.painter.paint(&req, &h.ctx).expect("paint");
    let seen = mock.seen();
    assert_eq!(seen[0].path, "/v1/images/edits");
    assert_eq!(seen[0].count("name=\"image[]\""), 3);
    assert_eq!(seen[0].count("name=\"mask\""), 1);
    assert_eq!(seen[0].count("Content-Type: image/png"), 4);
    assert!(seen[0].body.find("filename=\"canvas.png\"") < seen[0].body.find("filename=\"reference-1.png\""));
    assert_eq!(seen[0].field("input_fidelity").as_deref(), Some("high"));
    assert_eq!(seen[0].field("background").as_deref(), Some("opaque"));
    assert_eq!(seen[0].field("size").as_deref(), Some("1536x1024"));
    assert_eq!(seen[0].field("output_format").as_deref(), Some("png"));
    assert_eq!(seen[0].field("n").as_deref(), Some("1"));

    // References only: no mask, no background, no input_fidelity on gpt-image-2.
    let mut req = request(false, false, 2);
    req.model = "gpt-image-2".into();
    h.painter.paint(&req, &h.ctx).expect("paint");
    let seen = mock.seen();
    assert_eq!(seen[1].count("name=\"image[]\""), 2);
    assert_eq!(seen[1].count("name=\"mask\""), 0);
    assert_eq!(seen[1].field("input_fidelity"), None);
    assert_eq!(seen[1].field("background"), None);
}

#[test]
fn fallback_walk_is_remembered() {
    let mock = serve(|r| match r.model().as_str() {
        "gpt-image-2" => reply(200, ok_body(1536, 1024)),
        _ => reply(404, MODEL_404),
    });
    let h = Harness::new(&mock, "fallback");
    let req = request(false, false, 0);
    let r = h.painter.paint(&req, &h.ctx).expect("paint");
    assert_eq!(r.model_used, "gpt-image-2");
    let models: Vec<String> = mock.seen().iter().map(Captured::model).collect();
    assert_eq!(models, ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare", "gpt-image-2"]);
    assert_eq!(h.painter.working_model().as_deref(), Some("gpt-image-2"));
    let notes = h.log.of_kind(LogKind::Note);
    assert!(notes.iter().any(|n| n.contains("falling back to gpt-image-2.5-flare")), "{notes:?}");
    let ledger = h.ledger();
    assert_eq!(ledger.len(), 3);
    assert_eq!(ledger[0]["error_class"], "model_unavailable");
    assert_eq!(ledger[2]["outcome"], "ok");
    assert_eq!(ledger[2]["attempt"], 3);
    assert_eq!(ledger[2]["model_requested"], "gpt-image-2.5-sunburst");
    assert_eq!(ledger[2]["model_used"], "gpt-image-2");

    // The next call goes straight to the model that worked.
    let r = h.painter.paint(&req, &h.ctx).expect("paint");
    assert_eq!(r.model_used, "gpt-image-2");
    assert_eq!(mock.seen().len(), 4);
}

#[test]
fn standard_size_fallback_resizes_result() {
    let mock = serve(|r| match r.model().as_str() {
        "gpt-image-1" => reply(200, ok_body(1536, 1024)),
        _ => reply(404, MODEL_404),
    });
    let h = Harness::new(&mock, "stdsize");
    let mut req = request(true, true, 0);
    req.width = 1920;
    req.height = 1088;
    req.canvas = Some(RgbaImage::from_pixel(1920, 1088, Rgba([1, 2, 3, 255])));
    req.mask = req.canvas.clone();
    req.quality = Quality::XHigh;
    let r = h.painter.paint(&req, &h.ctx).expect("paint");
    assert_eq!(r.model_used, "gpt-image-1");
    assert_eq!(r.image.dimensions(), (1920, 1088));
    let seen = mock.seen();
    assert_eq!(seen.len(), 5);
    let last = seen.last().unwrap();
    assert_eq!(last.field("size").as_deref(), Some("1536x1024"));
    assert_eq!(last.field("quality").as_deref(), Some("high"));
    assert_eq!(last.field("input_fidelity").as_deref(), Some("high"));
    assert_eq!(seen[0].field("size").as_deref(), Some("1920x1088"));
    assert_eq!(seen[0].field("quality").as_deref(), Some("xhigh"));
    let notes = h.log.of_kind(LogKind::Note);
    assert!(notes.iter().any(|n| n.contains("resized the 1536x1024 result")), "{notes:?}");
}

#[test]
fn no_model_available() {
    let mock = serve(|_| reply(404, MODEL_404));
    let h = Harness::new(&mock, "nomodel");
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(err, PaintError::ModelUnavailable(_)), "{err}");
    assert_eq!(mock.seen().len(), 5);
}

#[test]
fn transient_errors_are_retried() {
    let calls = Arc::new(Mutex::new(0));
    let c = Arc::clone(&calls);
    let mock = serve(move |_| {
        let mut n = lock(&c);
        *n += 1;
        if *n <= 2 {
            reply(500, SERVER_500)
        } else {
            reply(200, ok_body(1536, 1024))
        }
    });
    let h = Harness::new(&mock, "retry");
    let r = h.painter.paint(&request(false, false, 0), &h.ctx).expect("paint");
    assert_eq!(r.model_used, "gpt-image-2.5-sunburst");
    let ledger = h.ledger();
    assert_eq!(ledger.len(), 3);
    assert_eq!(ledger[0]["error_class"], "transient");
    assert_eq!(ledger[0]["http_status"], 500);
    assert_eq!(ledger[2]["outcome"], "ok");
}

#[test]
fn transient_gives_up_after_three_retries() {
    let mock = serve(|_| reply(503, SERVER_500));
    let h = Harness::new(&mock, "giveup");
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(err, PaintError::Transient(_)), "{err}");
    assert_eq!(mock.seen().len(), 4);
}

#[test]
fn quota_and_moderation_are_not_retried() {
    let quota = r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
    let mock = serve(move |_| reply(429, quota));
    let h = Harness::new(&mock, "quota");
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(&err, PaintError::Fatal(m) if m.contains("quota")), "{err}");
    assert_eq!(mock.seen().len(), 1);

    let refused = r#"{"error":{"message":"Your request was rejected by the safety system.","type":"image_generation_user_error","param":null,"code":"moderation_blocked"}}"#;
    let mock = serve(move |_| reply(400, refused));
    let h = Harness::new(&mock, "refused");
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(&err, PaintError::Refused(m) if m.contains("safety system")), "{err}");
    assert_eq!(mock.seen().len(), 1);
    assert_eq!(h.ledger()[0]["outcome"], "refused");
}

#[test]
fn cancel_in_flight_returns_fast_and_ledgers_late_result() {
    let mock = serve(|_| Reply { delay: Duration::from_millis(1500), ..reply(200, ok_body(1536, 1024)) });
    let h = Harness::new(&mock, "cancel");
    let cancel = Arc::clone(&h.ctx.cancel);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(err, PaintError::Cancelled), "{err}");
    assert!(t.elapsed() < Duration::from_millis(900), "{:?}", t.elapsed());
    assert!(h.log.of_kind(LogKind::Note).iter().any(|n| n.contains("may still finish it and bill")));
    // The late reply is ledgered by the helper thread when it lands.
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.ledger().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let ledger = h.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0]["error_class"], "cancelled");
    assert_eq!(ledger[0]["http_status"], 200);
    assert!(ledger[0]["estimated_usd"].as_f64().unwrap() > 0.0);
}

#[test]
fn cancel_during_backoff() {
    let mock = serve(|_| reply(500, SERVER_500));
    let mut h = Harness::new(&mock, "cancelbackoff");
    h.painter.retry_base = Duration::from_secs(5);
    let cancel = Arc::clone(&h.ctx.cancel);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(err, PaintError::Cancelled), "{err}");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(mock.seen().len(), 1);
}

#[test]
fn already_cancelled_makes_no_request() {
    let mock = serve(|_| reply(200, ok_body(1536, 1024)));
    let h = Harness::new(&mock, "precancel");
    h.ctx.cancel.store(true, Ordering::Relaxed);
    let err = h.painter.paint(&request(false, false, 0), &h.ctx).unwrap_err();
    assert!(matches!(err, PaintError::Cancelled));
    assert!(mock.seen().is_empty());
}
