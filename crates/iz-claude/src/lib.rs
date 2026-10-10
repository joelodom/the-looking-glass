//! Describer over the headless `claude` CLI.
//!
//! One describe call = one `claude -p` child process: the director's brief
//! as the system prompt, the request on stdin, `--json-schema` for the
//! SceneSpec shape, and only the Read tool so Claude can look at the frame
//! PNGs in the world directory (the child's working directory).

pub mod cli;
mod discover;
pub mod parse;
pub mod prompts;

use iz_types::{
    CallContext, CanvasMode, DescribeError, DescribeKind, DescribeRequest, Describer, LogEvent,
    LogKind, SceneSpec, Transition,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const SERVICE: &str = "claude";
/// A describe call that takes longer than this is abandoned as transient.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The real describer. Cheap to construct; the binary is found lazily on
/// the first call (and settings / `IZ_CLAUDE_BIN` are re-read every call).
#[derive(Default)]
pub struct ClaudeCli {
    discovered_bin: Mutex<Option<PathBuf>>,
    system_prompt_logged: AtomicBool,
}

impl ClaudeCli {
    pub fn new() -> Self {
        Self::default()
    }

    /// The binary this describer would run with these settings.
    pub fn binary(&self, settings: &iz_types::Settings) -> Result<PathBuf, DescribeError> {
        discover::resolve(settings.claude_bin.as_deref(), &self.discovered_bin)
            .map_err(DescribeError::Fatal)
    }
}

impl Describer for ClaudeCli {
    fn describe(
        &self,
        req: &DescribeRequest,
        ctx: &CallContext,
    ) -> Result<SceneSpec, DescribeError> {
        if ctx.cancelled() {
            return Err(DescribeError::Cancelled);
        }
        let purpose = prompts::purpose(&req.kind, &req.transition);
        let bin = self.binary(&req.settings)?;
        let started_at = chrono::Local::now();
        let user_prompt = prompts::user_prompt(req, started_at);
        let images: Vec<String> = prompts::image_list(req)
            .iter()
            .map(|(_, p)| p.display().to_string())
            .collect();

        let first = !self.system_prompt_logged.swap(true, Ordering::Relaxed);
        ctx.log(LogEvent::new(
            SERVICE,
            &purpose,
            LogKind::Request,
            request_log(first, &user_prompt, &images),
        ));

        std::fs::create_dir_all(&req.world_dir).map_err(|e| {
            DescribeError::Fatal(format!(
                "cannot use world directory {}: {e}",
                req.world_dir.display()
            ))
        })?;
        let schema = SceneSpec::json_schema().to_string();
        let args = cli::args(&req.settings.claude_model, prompts::SYSTEM_PROMPT, &schema);
        let cmd = cli::command(&bin, &args, &req.world_dir);

        let mut record = CallRecord {
            purpose: &purpose,
            started_at: started_at.to_rfc3339(),
            bin: &bin,
            model: &req.settings.claude_model,
            user_prompt: &user_prompt,
            images: &images,
            elapsed_ms: 0,
            exit_code: None,
            stderr: String::new(),
            reply: Value::Null,
            outcome: String::new(),
        };

        let finished = match cli::run(cmd, &user_prompt, || ctx.cancelled(), TIMEOUT) {
            Ok(f) => f,
            Err(e) => {
                let (err, elapsed) = match e {
                    cli::RunError::Spawn(e) => (
                        DescribeError::Fatal(format!("could not start {}: {e}", bin.display())),
                        Duration::ZERO,
                    ),
                    cli::RunError::Wait(e) => (
                        DescribeError::Transient(format!("lost the claude process: {e}")),
                        Duration::ZERO,
                    ),
                    cli::RunError::Cancelled(d) => (DescribeError::Cancelled, d),
                    cli::RunError::TimedOut(d) => (
                        DescribeError::Transient(format!(
                            "claude did not answer within {} s",
                            d.as_secs()
                        )),
                        d,
                    ),
                };
                record.elapsed_ms = elapsed.as_millis();
                record.outcome = err.to_string();
                record.write(ctx);
                let kind = if matches!(err, DescribeError::Cancelled) {
                    LogKind::Note
                } else {
                    LogKind::Error
                };
                ctx.log(LogEvent::new(
                    SERVICE,
                    &purpose,
                    kind,
                    format!("{err} after {:.1} s", elapsed.as_secs_f64()),
                ));
                return Err(err);
            }
        };

        record.elapsed_ms = finished.elapsed.as_millis();
        record.exit_code = finished.status.code();
        record.stderr = finished.stderr.clone();
        let envelope = parse::envelope(&finished.stdout);
        record.reply = envelope
            .clone()
            .unwrap_or_else(|| Value::String(finished.stdout.clone()));
        ctx.log(LogEvent::new(
            SERVICE,
            &purpose,
            LogKind::Response,
            response_log(&finished, envelope.as_ref()),
        ));

        let result = parse::interpret(
            &finished.stdout,
            &finished.stderr,
            finished.status.success(),
        );
        record.outcome = match &result {
            Ok((_, source)) => format!("ok ({source:?})"),
            Err(e) => e.to_string(),
        };
        record.write(ctx);

        match result {
            Ok((spec, source)) => {
                ctx.log(LogEvent::new(
                    SERVICE,
                    &purpose,
                    LogKind::Note,
                    summary(&finished, envelope.as_ref(), source),
                ));
                Ok(finalize(spec, req))
            }
            Err(e) => {
                ctx.log(LogEvent::new(
                    SERVICE,
                    &purpose,
                    LogKind::Error,
                    e.to_string(),
                ));
                Err(e)
            }
        }
    }
}

fn request_log(first: bool, user_prompt: &str, images: &[String]) -> String {
    let system = if first {
        format!("--- system prompt ---\n{}", prompts::SYSTEM_PROMPT)
    } else {
        format!(
            "--- system prompt: unchanged ({} chars, logged in full on the first call) ---",
            prompts::SYSTEM_PROMPT.len()
        )
    };
    let images = if images.is_empty() {
        "(none)".to_string()
    } else {
        images.join("\n")
    };
    format!("{system}\n\n--- user prompt ---\n{user_prompt}\n--- images ---\n{images}")
}

fn response_log(f: &cli::Finished, envelope: Option<&Value>) -> String {
    let body = match envelope {
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_else(|_| f.stdout.clone()),
        None => f.stdout.clone(),
    };
    let mut text = format!(
        "exit {:?} after {:.1} s\n{body}",
        f.status.code(),
        f.elapsed.as_secs_f64()
    );
    if !f.stderr.trim().is_empty() {
        text.push_str("\n--- stderr ---\n");
        text.push_str(f.stderr.trim());
    }
    text
}

fn summary(f: &cli::Finished, envelope: Option<&Value>, source: parse::Source) -> String {
    let field = |k: &str| {
        envelope
            .and_then(|e| e.get(k))
            .cloned()
            .unwrap_or(Value::Null)
    };
    format!(
        "answered in {:.1} s, {} turns, cost ${}, spec from {}",
        f.elapsed.as_secs_f64(),
        field("num_turns"),
        field("total_cost_usd"),
        match source {
            parse::Source::StructuredOutput => "structured_output",
            parse::Source::ResultText => "the result text (no structured_output)",
        }
    )
}

/// Light, conservative clean-up of what Claude returned: never invent
/// content, only repair what would break the pipeline or lose the dossier.
pub fn finalize(mut spec: SceneSpec, req: &DescribeRequest) -> SceneSpec {
    let previous = match &req.kind {
        DescribeKind::Rewrite { previous, .. } => Some(previous),
        _ => None,
    };
    let fallback_cam = previous
        .map(|p| &p.camera)
        .or(req.proposed_camera.as_ref())
        .or(req.prior_spec.as_ref().map(|s| &s.camera));

    let cam = &mut spec.camera;
    if !(cam.frame_width_m.is_finite() && cam.frame_width_m > 0.0) {
        cam.frame_width_m = fallback_cam.map(|c| c.frame_width_m).unwrap_or(100.0);
    }
    if !(cam.distance_m.is_finite() && cam.distance_m > 0.0) {
        cam.distance_m = fallback_cam
            .map(|c| c.distance_m)
            .unwrap_or(cam.frame_width_m);
    }
    if !cam.tilt_deg.is_finite() {
        cam.tilt_deg = fallback_cam.map(|c| c.tilt_deg).unwrap_or(0.0);
    }
    cam.tilt_deg = cam.tilt_deg.clamp(0.0, 180.0);
    cam.heading_deg = if cam.heading_deg.is_finite() {
        iz_types::transition::wrap_heading(cam.heading_deg)
    } else {
        0.0
    };
    if cam.regime.trim().is_empty() {
        cam.regime = fallback_cam
            .map(|c| c.regime.clone())
            .unwrap_or_else(|| "geographic".into());
    }
    if cam.scale_label.trim().is_empty() {
        cam.scale_label = prompts::format_scale(cam.frame_width_m);
    }

    if !matches!(req.transition, Transition::ZoomIn { .. }) {
        spec.canvas_mode = CanvasMode::Reference;
    }
    if spec.title.trim().is_empty() {
        spec.title = previous
            .map(|p| p.title.clone())
            .unwrap_or_else(|| req.transition.label());
    }

    let prior_dossier = previous
        .map(|p| &p.dossier)
        .or(req.prior_spec.as_ref().map(|s| &s.dossier));
    spec.dossier = keep_dossier(std::mem::take(&mut spec.dossier), prior_dossier);
    spec
}

/// Claude owns the dossier, but a reply that is not an object, or that
/// forgot `established_facts` altogether, must not wipe what came before.
fn keep_dossier(new: Value, prior: Option<&Value>) -> Value {
    let prior_obj = prior.and_then(Value::as_object);
    let mut new = match new {
        Value::Object(m) => m,
        _ => {
            return prior_obj
                .map(|m| Value::Object(m.clone()))
                .unwrap_or_else(|| json!({}))
        }
    };
    if let Some(facts) = prior_obj.and_then(|m| m.get("established_facts")) {
        new.entry("established_facts")
            .or_insert_with(|| facts.clone());
    }
    Value::Object(new)
}

/// What goes into `<record_dir>/<purpose>-<timestamp>.json`.
struct CallRecord<'a> {
    purpose: &'a str,
    started_at: String,
    bin: &'a Path,
    model: &'a str,
    user_prompt: &'a str,
    images: &'a [String],
    elapsed_ms: u128,
    exit_code: Option<i32>,
    stderr: String,
    reply: Value,
    outcome: String,
}

impl CallRecord<'_> {
    fn write(&self, ctx: &CallContext) {
        let Some(dir) = &ctx.record_dir else { return };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
        let path = dir.join(format!("{}-{stamp}.json", prompts::slug(self.purpose)));
        let body = json!({
            "service": SERVICE,
            "purpose": self.purpose,
            "started_at": self.started_at,
            "elapsed_ms": self.elapsed_ms as u64,
            "bin": self.bin.display().to_string(),
            "model": self.model,
            "max_turns": cli::MAX_TURNS,
            "system_prompt": prompts::SYSTEM_PROMPT,
            "user_prompt": self.user_prompt,
            "images": self.images,
            "exit_code": self.exit_code,
            "stderr": self.stderr,
            "reply": self.reply,
            "outcome": self.outcome,
        });
        let written = std::fs::create_dir_all(dir).and_then(|_| {
            std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap_or_default())
        });
        if let Err(e) = written {
            ctx.log(LogEvent::new(
                SERVICE,
                self.purpose,
                LogKind::Error,
                format!("could not write call record {}: {e}", path.display()),
            ));
        }
    }
}

#[cfg(test)]
mod tests;
