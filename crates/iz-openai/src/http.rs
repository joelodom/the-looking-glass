//! One HTTP exchange with the images API, run on a helper thread so the
//! caller can give up on cancel without waiting for the socket.

use crate::classify::{classify_transport, ErrorClass, Failure};
use reqwest::blocking::{multipart, Client};
use serde_json::Value;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// High quality at large sizes can take minutes.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
/// How often the waiting thread checks the cancel flag.
pub const POLL: Duration = Duration::from_millis(100);

pub fn build_client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .user_agent(concat!("the-looking-glass/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("could not build the HTTP client: {e}"))
}

/// PNG-encoded inputs for the edits endpoint, shared across retries.
#[derive(Debug, Default)]
pub struct EncodedInputs {
    /// (file name, PNG bytes), canvas first when present.
    pub images: Vec<(String, Vec<u8>)>,
    pub mask: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum Body {
    Json(Value),
    Multipart { fields: Vec<(&'static str, String)>, inputs: Arc<EncodedInputs> },
}

#[derive(Debug, Clone)]
pub struct HttpSpec {
    pub url: String,
    pub key: String,
    pub body: Body,
}

#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: String,
}

/// What one exchange produced, plus how long it took.
pub type Exchange = (Result<RawResponse, Failure>, Duration);

fn png_part(name: &str, bytes: &[u8]) -> Result<multipart::Part, Failure> {
    multipart::Part::bytes(bytes.to_vec())
        .file_name(name.to_string())
        .mime_str("image/png")
        .map_err(|e| Failure { class: ErrorClass::Fatal, message: format!("multipart: {e}"), status: None })
}

pub fn build_form(
    fields: &[(&'static str, String)],
    inputs: &EncodedInputs,
) -> Result<multipart::Form, Failure> {
    let mut form = multipart::Form::new();
    for (name, value) in fields {
        form = form.text(*name, value.clone());
    }
    for (file_name, bytes) in &inputs.images {
        form = form.part("image[]", png_part(file_name, bytes)?);
    }
    if let Some(mask) = &inputs.mask {
        form = form.part("mask", png_part("mask.png", mask)?);
    }
    Ok(form)
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(ms) = get("retry-after-ms").and_then(|v| v.parse::<f64>().ok()) {
        return Some(Duration::from_secs_f64(ms.max(0.0) / 1000.0));
    }
    get("retry-after").and_then(|v| v.parse::<f64>().ok()).map(|s| Duration::from_secs_f64(s.max(0.0)))
}

/// Send one request and read the whole reply. Blocking.
pub fn exchange(client: &Client, spec: &HttpSpec) -> Result<RawResponse, Failure> {
    let builder = client.post(&spec.url).bearer_auth(&spec.key);
    let builder = match &spec.body {
        Body::Json(v) => builder.json(v),
        Body::Multipart { fields, inputs } => builder.multipart(build_form(fields, inputs)?),
    };
    let resp = builder.send().map_err(|e| classify_transport(&e.without_url()))?;
    let status = resp.status().as_u16();
    let retry_after = parse_retry_after(resp.headers());
    let body = resp.text().map_err(|e| classify_transport(&e.without_url()))?;
    Ok(RawResponse { status, retry_after, body })
}

#[derive(Default)]
struct Slot {
    result: Option<Exchange>,
    /// The caller stopped waiting; the helper owns the late result.
    abandoned: bool,
}

/// Run `exchange` on a helper thread. Returns `None` if `cancelled()` became
/// true first; in that case `on_abandoned` runs on the helper thread with the
/// late result once it arrives (for ledgering a call that may still be
/// billed). Exactly one of the two paths sees the result.
pub fn run_cancellable(
    client: &Client,
    spec: HttpSpec,
    cancelled: impl Fn() -> bool,
    on_abandoned: impl FnOnce(Exchange) + Send + 'static,
) -> Option<Exchange> {
    let shared = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
    let helper_shared = Arc::clone(&shared);
    let client = client.clone();
    let spawned = std::thread::Builder::new().name("iz-openai-http".into()).spawn(move || {
        let started = Instant::now();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| exchange(&client, &spec)))
            .unwrap_or_else(|_| {
                Err(Failure {
                    class: ErrorClass::Transient,
                    message: "the HTTP helper thread panicked".into(),
                    status: None,
                })
            });
        let done = (result, started.elapsed());
        let (lock, cv) = &*helper_shared;
        let mut slot = lock.lock().unwrap_or_else(|e| e.into_inner());
        if slot.abandoned {
            drop(slot);
            on_abandoned(done);
        } else {
            slot.result = Some(done);
            cv.notify_all();
        }
    });
    if let Err(e) = spawned {
        let failure =
            Failure { class: ErrorClass::Transient, message: format!("could not start a thread: {e}"), status: None };
        return Some((Err(failure), Duration::ZERO));
    }

    let (lock, cv) = &*shared;
    let mut slot = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(done) = slot.result.take() {
            return Some(done);
        }
        if cancelled() {
            slot.abandoned = true;
            return None;
        }
        slot = cv.wait_timeout(slot, POLL).unwrap_or_else(|e| e.into_inner()).0;
    }
}
