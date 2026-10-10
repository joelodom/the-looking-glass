//! Painter over the OpenAI images API (`/v1/images/generations` and
//! `/v1/images/edits`), with model fallback, retries, cancellation, a JSONL
//! spend ledger and per-call records.

mod classify;
mod http;
mod ledger;
mod models;
mod plan;
mod pricing;

pub use models::FALLBACK_MODELS;
pub use pricing::RATES_AS_OF;

use crate::classify::{excerpt, ErrorClass, Failure};
use crate::http::{EncodedInputs, Exchange, HttpSpec, RawResponse};
use crate::ledger::LedgerEntry;
use crate::plan::{route, Endpoint, Plan};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use chrono::{DateTime, SecondsFormat, Utc};
use image::RgbaImage;
use iz_types::settings::{load_dotenv, paths};
use iz_types::{imageops, CallContext, LogEvent, LogKind, PaintError, PaintRequest, PaintResult, Painter};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const SERVICE: &str = "openai";
/// Retries after the first attempt, for transient failures only.
const MAX_RETRIES: u32 = 3;
const MAX_INPUT_IMAGES: usize = 16;
const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_MASK_BYTES: usize = 4 * 1024 * 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Paints frames with OpenAI's image models. Cheap to construct; the HTTP
/// client is built on first use and a missing API key is reported at paint
/// time.
pub struct OpenAiPainter {
    base_url: String,
    api_key: Option<String>,
    ledger_path: Option<PathBuf>,
    retry_base: Duration,
    client: OnceLock<Result<Client, String>>,
    /// Models that answered "does not exist / no access" in this process.
    unavailable: Mutex<Vec<String>>,
    /// The last model that produced an image.
    working: Mutex<Option<String>>,
}

impl OpenAiPainter {
    /// Loads `.env` (existing environment variables win). Never fails.
    pub fn new() -> Self {
        load_dotenv();
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: None,
            ledger_path: None,
            retry_base: Duration::from_secs(1),
            client: OnceLock::new(),
            unavailable: Mutex::new(Vec::new()),
            working: Mutex::new(None),
        }
    }

    /// Point at another OpenAI-compatible base URL (e.g. a local test server).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// Use this key instead of `OPENAI_API_KEY`.
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Write the ledger here instead of `paths::ledger_file()`.
    pub fn with_ledger_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.ledger_path = Some(path.into());
        self
    }

    /// The model that most recently produced an image, if any.
    pub fn working_model(&self) -> Option<String> {
        lock(&self.working).clone()
    }

    fn ledger_path(&self) -> PathBuf {
        self.ledger_path.clone().unwrap_or_else(paths::ledger_file)
    }

    fn client(&self) -> Result<&Client, PaintError> {
        self.client
            .get_or_init(http::build_client)
            .as_ref()
            .map_err(|e| PaintError::Fatal(e.clone()))
    }

    fn key(&self) -> Result<String, PaintError> {
        resolve_key(self.api_key.as_deref(), std::env::var(iz_types::env::OPENAI_API_KEY).ok().as_deref())
    }
}

impl Default for OpenAiPainter {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn resolve_key(explicit: Option<&str>, env: Option<&str>) -> Result<String, PaintError> {
    explicit
        .or(env)
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            PaintError::Fatal(
                "OPENAI_API_KEY is not set: add OPENAI_API_KEY=... to the .env file at the workspace root \
                 (or the app data directory), or export it"
                    .into(),
            )
        })
}

fn note(ctx: &CallContext, req: &PaintRequest, text: impl Into<String>) {
    ctx.log(LogEvent::new(SERVICE, &req.purpose, LogKind::Note, text));
}

/// Checks that need no network.
fn validate(req: &PaintRequest) -> Result<(), PaintError> {
    let fatal = |m: String| Err(PaintError::Fatal(m));
    if req.width == 0 || req.height == 0 {
        return fatal(format!("invalid frame size {}x{}", req.width, req.height));
    }
    if req.prompt.trim().is_empty() {
        return fatal("empty prompt".into());
    }
    match (&req.canvas, &req.mask) {
        (None, Some(_)) => fatal("a mask was given without a canvas".into()),
        (Some(c), Some(m)) if c.dimensions() != m.dimensions() => fatal(format!(
            "mask is {}x{} but the canvas is {}x{}; they must match",
            m.width(),
            m.height(),
            c.width(),
            c.height()
        )),
        _ => Ok(()),
    }
}

/// PNG-encode the edit inputs for an API size: canvas (alpha kept, resized
/// to the API size if needed), references (made opaque), mask (alpha kept).
fn encode_inputs(req: &PaintRequest, api: (u32, u32)) -> Result<(EncodedInputs, Vec<String>), PaintError> {
    let encode = |img: &RgbaImage, what: &str| {
        imageops::encode_png(img).map_err(|e| PaintError::Fatal(format!("could not encode the {what}: {e}")))
    };
    let mut notes = Vec::new();
    let mut images = Vec::new();
    if let Some(canvas) = &req.canvas {
        let canvas: Cow<RgbaImage> = if canvas.dimensions() == api {
            Cow::Borrowed(canvas)
        } else {
            notes.push(format!(
                "canvas resized from {}x{} to {}x{} for the API",
                canvas.width(),
                canvas.height(),
                api.0,
                api.1
            ));
            Cow::Owned(imageops::resize_to(canvas, api.0, api.1))
        };
        images.push(("canvas.png".to_string(), encode(&canvas, "canvas")?));
    }
    let room = MAX_INPUT_IMAGES - images.len();
    if req.references.len() > room {
        notes.push(format!(
            "the API takes at most {MAX_INPUT_IMAGES} images; dropping {} reference(s)",
            req.references.len() - room
        ));
    }
    for (i, r) in req.references.iter().take(room).enumerate() {
        images.push((format!("reference-{}.png", i + 1), encode(&imageops::opaque(r), "reference")?));
    }
    for (name, bytes) in &images {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(PaintError::Fatal(format!(
                "{name} is {} MB as PNG; the API limit is 50 MB",
                bytes.len() / (1024 * 1024)
            )));
        }
    }
    let mask = match &req.mask {
        None => None,
        Some(mask) => {
            let mask: Cow<RgbaImage> = if mask.dimensions() == api {
                Cow::Borrowed(mask)
            } else {
                // Nearest keeps the alpha edge hard.
                Cow::Owned(image::imageops::resize(mask, api.0, api.1, image::imageops::FilterType::Nearest))
            };
            let bytes = encode(&mask, "mask")?;
            if bytes.len() > MAX_MASK_BYTES {
                return Err(PaintError::Fatal(format!(
                    "the mask is {} KB as PNG; the API limit is 4 MB",
                    bytes.len() / 1024
                )));
            }
            Some(bytes)
        }
    };
    Ok((EncodedInputs { images, mask }, notes))
}

/// A reply as the ledger and the retry loop see it. `T` is the PNG bytes,
/// then the decoded image.
struct Interpreted<T> {
    outcome: Result<T, Failure>,
    status: Option<u16>,
    usage: Option<Value>,
    retry_after: Option<Duration>,
}

impl<T> Interpreted<T> {
    fn and_then<U>(self, f: impl FnOnce(T) -> Result<U, Failure>) -> Interpreted<U> {
        Interpreted {
            outcome: self.outcome.and_then(f),
            status: self.status,
            usage: self.usage,
            retry_after: self.retry_after,
        }
    }
}

/// Status, error classification, usage and the base64 image of one reply.
fn interpret(res: Result<RawResponse, Failure>) -> Interpreted<Vec<u8>> {
    let raw = match res {
        Ok(raw) => raw,
        Err(f) => return Interpreted { outcome: Err(f), status: None, usage: None, retry_after: None },
    };
    let status = Some(raw.status);
    let retry_after = raw.retry_after;
    if !(200..300).contains(&raw.status) {
        let failure = classify::classify(raw.status, &raw.body);
        return Interpreted { outcome: Err(failure), status, usage: None, retry_after };
    }
    let fatal = |message: String| Failure { class: ErrorClass::Fatal, message, status };
    let v: Value = match serde_json::from_str(&raw.body) {
        Ok(v) => v,
        Err(e) => {
            let f = fatal(format!("the reply was not JSON ({e}): {}", excerpt(&raw.body, 200)));
            return Interpreted { outcome: Err(f), status, usage: None, retry_after };
        }
    };
    let usage = v.get("usage").filter(|u| !u.is_null()).cloned();
    let outcome = match v.pointer("/data/0/b64_json").and_then(Value::as_str) {
        None => Err(fatal(format!("the reply had no image (data[0].b64_json): {}", excerpt(&raw.body, 300)))),
        Some(b64) => BASE64_STANDARD
            .decode(b64)
            .map_err(|e| fatal(format!("the image was not valid base64: {e}"))),
    };
    Interpreted { outcome, status, usage, retry_after }
}

/// Fill in a ledger line for one finished attempt.
fn complete_entry<T>(template: &LedgerEntry, it: &Interpreted<T>, elapsed: Duration, cancelled: bool) -> LedgerEntry {
    let (outcome, class, error) = match (&it.outcome, cancelled) {
        (_, true) => ("error", Some("cancelled".to_string()), Some("cancelled by the caller; result discarded".into())),
        (Ok(_), false) => ("ok", None, None),
        (Err(f), false) if f.class == ErrorClass::Refused => {
            ("refused", Some(f.class.as_str().to_string()), Some(f.message.clone()))
        }
        (Err(f), false) => ("error", Some(f.class.as_str().to_string()), Some(f.message.clone())),
    };
    LedgerEntry {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        outcome: outcome.to_string(),
        error_class: class,
        http_status: it.status,
        error,
        elapsed_ms: elapsed.as_millis() as u64,
        usage: it.usage.clone(),
        estimated_usd: pricing::estimate_usd(&template.model_used, it.usage.as_ref()),
        ..template.clone()
    }
}

/// Exponential backoff with jitter, stretched to honor `Retry-After`.
fn backoff_delay(base: Duration, retry: u32, retry_after: Option<Duration>) -> Duration {
    let exp = base.saturating_mul(1u32 << retry.min(10));
    let jitter_ms = rand::random_range(0..=base.as_millis().max(1) as u64);
    let delay = exp + Duration::from_millis(jitter_ms);
    retry_after.map_or(delay, |ra| delay.max(ra)).min(MAX_BACKOFF)
}

fn sleep_cancellable(total: Duration, ctx: &CallContext) -> Result<(), PaintError> {
    let deadline = Instant::now() + total;
    loop {
        if ctx.cancelled() {
            return Err(PaintError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        std::thread::sleep((deadline - now).min(http::POLL));
    }
}

/// How trying one model ended, when it did not produce an image.
enum ModelFailure {
    Unavailable(String),
    Paint(PaintError),
}

/// State of one `paint` call, for the ledger and the record file.
struct CallState {
    started: Instant,
    started_utc: DateTime<Utc>,
    attempt: u32,
    attempts: Vec<LedgerEntry>,
    /// The plan of the model that produced the image.
    final_plan: Option<Plan>,
}

impl CallState {
    fn new() -> Self {
        Self { started: Instant::now(), started_utc: Utc::now(), attempt: 0, attempts: Vec::new(), final_plan: None }
    }
}

fn request_text(url: &str, req: &PaintRequest, plan: &Plan, inputs: Option<&EncodedInputs>) -> String {
    let mut t = format!("POST {url}\nmodel: {}", plan.model);
    if plan.model != req.model {
        t += &format!(" (requested {})", req.model);
    }
    t += &format!("\nsize: {}", plan.size());
    if plan.api_size() != (req.width, req.height) {
        t += &format!(" (requested {}x{}; result resized)", req.width, req.height);
    }
    t += &format!("\nquality: {}", plan.quality);
    if plan.endpoint == Endpoint::Edits {
        let n = inputs.map_or(0, |i| i.images.len());
        let canvas = if req.canvas.is_some() { "canvas + " } else { "" };
        let refs = n - usize::from(req.canvas.is_some());
        t += &format!(
            "\nimages: {n} ({canvas}{refs} reference(s)), mask: {}",
            if req.mask.is_some() { "yes" } else { "no" }
        );
        if let Some(f) = plan.input_fidelity {
            t += &format!("\ninput_fidelity: {f}");
        }
    }
    t += &format!("\nprompt:\n{}", req.prompt);
    t
}

impl OpenAiPainter {
    fn paint_inner(
        &self,
        req: &PaintRequest,
        ctx: &CallContext,
        call: &mut CallState,
    ) -> Result<PaintResult, PaintError> {
        if ctx.cancelled() {
            return Err(PaintError::Cancelled);
        }
        validate(req)?;
        let key = self.key()?;
        let client = self.client()?;

        let candidates = models::candidate_models(&req.model, &lock(&self.unavailable));
        if candidates.first().map(String::as_str) != Some(req.model.trim()) {
            note(
                ctx,
                req,
                format!(
                    "{} was unavailable earlier in this session; starting with {}",
                    req.model, candidates[0]
                ),
            );
        }

        let mut encoded: Option<((u32, u32), Arc<EncodedInputs>)> = None;
        let mut skipped = Vec::new();
        for (i, model) in candidates.iter().enumerate() {
            let plan = Plan::new(req, model);
            for n in &plan.notes {
                note(ctx, req, n.clone());
            }
            let inputs = match plan.endpoint {
                Endpoint::Generations => None,
                Endpoint::Edits => match &encoded {
                    Some((size, inputs)) if *size == plan.api_size() => Some(Arc::clone(inputs)),
                    _ => {
                        let (inputs, notes) = encode_inputs(req, plan.api_size())?;
                        for n in notes {
                            note(ctx, req, n);
                        }
                        let inputs = Arc::new(inputs);
                        encoded = Some((plan.api_size(), Arc::clone(&inputs)));
                        Some(inputs)
                    }
                },
            };
            let url = format!("{}/images/{}", self.base_url, plan.endpoint.name());
            ctx.log(LogEvent::new(
                SERVICE,
                &req.purpose,
                LogKind::Request,
                request_text(&url, req, &plan, inputs.as_deref()),
            ));
            let spec = HttpSpec { url, key: key.clone(), body: plan.body(req, inputs) };
            match self.try_model(req, ctx, client, &plan, spec, call) {
                Ok((image, usage)) => {
                    *lock(&self.working) = Some(plan.model.clone());
                    let estimated_usd = pricing::estimate_usd(&plan.model, usage.as_ref());
                    let image = if image.dimensions() == (req.width, req.height) {
                        image
                    } else {
                        note(
                            ctx,
                            req,
                            format!(
                                "resized the {}x{} result to the requested {}x{}",
                                image.width(),
                                image.height(),
                                req.width,
                                req.height
                            ),
                        );
                        imageops::resize_to(&image, req.width, req.height)
                    };
                    let model_used = plan.model.clone();
                    call.final_plan = Some(plan);
                    return Ok(PaintResult { image, model_used, usage, estimated_usd });
                }
                Err(ModelFailure::Unavailable(msg)) => {
                    {
                        let mut dead = lock(&self.unavailable);
                        if !dead.contains(model) {
                            dead.push(model.clone());
                        }
                    }
                    let next = match candidates.get(i + 1) {
                        Some(next) => format!("falling back to {next}"),
                        None => "no fallbacks left".to_string(),
                    };
                    note(ctx, req, format!("{model} is not available to this account ({msg}); {next}"));
                    skipped.push(format!("{model}: {msg}"));
                }
                Err(ModelFailure::Paint(e)) => return Err(e),
            }
        }
        Err(PaintError::ModelUnavailable(format!("no image model worked. {}", skipped.join("; "))))
    }

    /// One model, with retries for transient failures.
    fn try_model(
        &self,
        req: &PaintRequest,
        ctx: &CallContext,
        client: &Client,
        plan: &Plan,
        spec: HttpSpec,
        call: &mut CallState,
    ) -> Result<(RgbaImage, Option<Value>), ModelFailure> {
        let ledger_path = self.ledger_path();

        for retry in 0..=MAX_RETRIES {
            if ctx.cancelled() {
                return Err(ModelFailure::Paint(PaintError::Cancelled));
            }
            call.attempt += 1;
            let template = LedgerEntry {
                ts: String::new(),
                purpose: req.purpose.clone(),
                endpoint: plan.endpoint.name().to_string(),
                model_requested: req.model.clone(),
                model_used: plan.model.clone(),
                size: plan.size(),
                requested_size: format!("{}x{}", req.width, req.height),
                quality: plan.quality.to_string(),
                attempt: call.attempt,
                outcome: String::new(),
                error_class: None,
                http_status: None,
                error: None,
                elapsed_ms: 0,
                usage: None,
                estimated_usd: None,
                usd_is_estimate: true,
                rates_as_of: RATES_AS_OF.to_string(),
            };

            let late = {
                let template = template.clone();
                let path = ledger_path.clone();
                let log = Arc::clone(&ctx.log);
                move |(res, elapsed): Exchange| {
                    let it = interpret(res);
                    let entry = complete_entry(&template, &it, elapsed, true);
                    ledger::append(&path, &entry);
                    let cost = entry.estimated_usd.map_or("unknown".into(), |c| format!("${c:.4} (estimate)"));
                    log.event(&LogEvent::new(
                        SERVICE,
                        &template.purpose,
                        LogKind::Note,
                        format!(
                            "the cancelled request finished after {:.1} s (HTTP {}); result discarded, \
                             cost {cost}, recorded in the ledger",
                            elapsed.as_secs_f64(),
                            entry.http_status.map_or("none".into(), |s| s.to_string()),
                        ),
                    ));
                }
            };
            let Some((res, elapsed)) = http::run_cancellable(client, spec.clone(), || ctx.cancelled(), late) else {
                note(
                    ctx,
                    req,
                    "cancelled while the request was in flight. OpenAI may still finish it and bill for it; \
                     the result will be discarded and recorded in the ledger when it arrives.",
                );
                return Err(ModelFailure::Paint(PaintError::Cancelled));
            };

            // Decode before ledgering so an unreadable image counts as an error.
            let it = interpret(res);
            let status = it.status;
            let it = it.and_then(|png| {
                imageops::decode(&png).map_err(|e| Failure {
                    class: ErrorClass::Fatal,
                    message: format!("could not decode the returned image: {e}"),
                    status,
                })
            });
            let entry = complete_entry(&template, &it, elapsed, false);
            ledger::append(&ledger_path, &entry);
            call.attempts.push(entry);

            let failure = match it.outcome {
                Ok(image) => return Ok((image, it.usage)),
                Err(f) => f,
            };
            match failure.class {
                ErrorClass::Transient if retry < MAX_RETRIES => {
                    let delay = backoff_delay(self.retry_base, retry, it.retry_after);
                    note(
                        ctx,
                        req,
                        format!(
                            "{} failed ({}); retry {} of {MAX_RETRIES} in {:.1} s",
                            plan.model,
                            failure.message,
                            retry + 1,
                            delay.as_secs_f64()
                        ),
                    );
                    sleep_cancellable(delay, ctx).map_err(ModelFailure::Paint)?;
                }
                ErrorClass::Transient => {
                    return Err(ModelFailure::Paint(PaintError::Transient(format!(
                        "{} (gave up after {} attempts)",
                        failure.message,
                        MAX_RETRIES + 1
                    ))))
                }
                ErrorClass::Refused => return Err(ModelFailure::Paint(PaintError::Refused(failure.message))),
                ErrorClass::ModelUnavailable => return Err(ModelFailure::Unavailable(failure.message)),
                ErrorClass::Fatal => return Err(ModelFailure::Paint(PaintError::Fatal(failure.message))),
            }
        }
        unreachable!("the last retry always returns")
    }

    /// Write `<purpose>-<timestamp>.json` (and `.png` on success) into
    /// `ctx.record_dir`. Returns the PNG path when one was written.
    fn write_record(
        &self,
        req: &PaintRequest,
        ctx: &CallContext,
        call: &CallState,
        result: &Result<PaintResult, PaintError>,
    ) -> Option<PathBuf> {
        let dir = ctx.record_dir.as_ref()?;
        let stem = format!("{}-{}", file_stem(&req.purpose), call.started_utc.format("%Y%m%d-%H%M%S%.3f"));
        let json_path = dir.join(format!("{stem}.json"));
        let png_path = dir.join(format!("{stem}.png"));
        let (outcome, error) = match result {
            Ok(_) => ("ok", None),
            Err(PaintError::Refused(m)) => ("refused", Some(m.clone())),
            Err(PaintError::Cancelled) => ("cancelled", None),
            Err(e) => ("error", Some(e.to_string())),
        };
        let mut png_written = None;
        if let Ok(r) = result {
            match imageops::save_png(&r.image, &png_path) {
                Ok(()) => png_written = Some(png_path.clone()),
                Err(e) => note(ctx, req, format!("could not save the record image: {e}")),
            }
        }
        let plan = call.final_plan.as_ref();
        let record = json!({
            "service": SERVICE,
            "purpose": req.purpose,
            "started": call.started_utc.to_rfc3339_opts(SecondsFormat::Millis, true),
            "endpoint": route(req).name(),
            "model_requested": req.model,
            "model_used": result.as_ref().ok().map(|r| r.model_used.clone()),
            "requested_size": format!("{}x{}", req.width, req.height),
            "api_size": plan.map(Plan::size),
            "quality_requested": req.quality.as_str(),
            "quality_sent": plan.map(|p| p.quality),
            "canvas": req.canvas.is_some(),
            "mask": req.mask.is_some(),
            "references": req.references.len(),
            "prompt": req.prompt,
            "outcome": outcome,
            "error": error,
            "usage": result.as_ref().ok().and_then(|r| r.usage.clone()),
            "estimated_usd": result.as_ref().ok().and_then(|r| r.estimated_usd),
            "usd_is_estimate": true,
            "rates_as_of": RATES_AS_OF,
            "elapsed_ms": call.started.elapsed().as_millis() as u64,
            "attempts": call.attempts,
            "image_file": png_written.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()),
        });
        let text = serde_json::to_string_pretty(&record).unwrap_or_default();
        let written = std::fs::create_dir_all(dir)
            .and_then(|()| iz_types::settings::write_atomic(&json_path, text.as_bytes()));
        if let Err(e) = written {
            note(ctx, req, format!("could not write the call record {}: {e}", json_path.display()));
        }
        png_written
    }
}

/// A purpose like "paint zoom out" as a file-name-safe stem.
fn file_stem(purpose: &str) -> String {
    let s: String = purpose
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() {
        "paint".to_string()
    } else {
        s
    }
}

impl Painter for OpenAiPainter {
    fn paint(&self, req: &PaintRequest, ctx: &CallContext) -> Result<PaintResult, PaintError> {
        let mut call = CallState::new();
        let result = self.paint_inner(req, ctx, &mut call);
        let png = self.write_record(req, ctx, &call, &result);
        let elapsed = call.started.elapsed().as_secs_f64();
        match &result {
            Ok(r) => {
                let usage = r.usage.as_ref().map_or("none reported".into(), |u| u.to_string());
                let cost = r.estimated_usd.map_or("unknown".into(), |c| {
                    format!("${c:.4} (estimate, list prices as of {RATES_AS_OF})")
                });
                let image = png.map_or("not recorded".into(), |p| p.display().to_string());
                ctx.log(LogEvent::new(
                    SERVICE,
                    &req.purpose,
                    LogKind::Response,
                    format!(
                        "model used: {}\nsize: {}x{}\nelapsed: {elapsed:.1} s ({} attempt(s))\nusage: {usage}\n\
                         estimated cost: {cost}\nimage: {image}",
                        r.model_used,
                        r.image.width(),
                        r.image.height(),
                        call.attempts.len(),
                    ),
                ));
            }
            Err(PaintError::Cancelled) => note(ctx, req, format!("cancelled after {elapsed:.1} s")),
            Err(e) => ctx.log(LogEvent::new(
                SERVICE,
                &req.purpose,
                LogKind::Error,
                format!("{e} (after {elapsed:.1} s, {} attempt(s))", call.attempts.len()),
            )),
        }
        result
    }
}

#[cfg(test)]
mod tests;
