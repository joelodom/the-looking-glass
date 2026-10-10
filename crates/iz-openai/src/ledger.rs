//! Append-only JSONL record of every image API attempt, for spend tracking.
//! Writing the ledger never fails a call: problems are reported on stderr.

use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// One line of the ledger: one HTTP attempt.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LedgerEntry {
    /// UTC, RFC 3339 with milliseconds.
    pub ts: String,
    pub purpose: String,
    /// "generations" or "edits".
    pub endpoint: String,
    pub model_requested: String,
    /// The model sent on this attempt.
    pub model_used: String,
    /// The size sent to the API.
    pub size: String,
    /// The size the caller asked for (the result is resized to it).
    pub requested_size: String,
    pub quality: String,
    /// 1-based, counted across retries and fallbacks within one paint call.
    pub attempt: u32,
    /// "ok", "refused" or "error".
    pub outcome: String,
    /// For errors: "transient", "fatal", "model_unavailable", "cancelled".
    pub error_class: Option<String>,
    pub http_status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    pub usage: Option<Value>,
    pub estimated_usd: Option<f64>,
    /// Always true: `estimated_usd` comes from a hand-copied price table.
    pub usd_is_estimate: bool,
    pub rates_as_of: String,
}

impl LedgerEntry {
    /// The JSONL line, without the trailing newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| format!(r#"{{"ledger_serialize_error":"{e}"}}"#))
    }
}

/// Serializes appends within this process so lines never interleave.
static LEDGER_LOCK: Mutex<()> = Mutex::new(());

/// Append one line to the ledger at `path`, creating parent directories.
/// Never panics and never returns an error; failures go to stderr.
pub fn append(path: &Path, entry: &LedgerEntry) {
    let _guard = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        let mut line = entry.to_line();
        line.push('\n');
        file.write_all(line.as_bytes())
    })();
    if let Err(e) = result {
        eprintln!("iz-openai: warning: could not write the ledger at {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn sample() -> LedgerEntry {
        LedgerEntry {
            ts: "2026-10-09T20:41:00.123Z".into(),
            purpose: "paint zoom out".into(),
            endpoint: "edits".into(),
            model_requested: "gpt-image-2.5-sunburst".into(),
            model_used: "gpt-image-2".into(),
            size: "1536x1024".into(),
            requested_size: "1536x1024".into(),
            quality: "high".into(),
            attempt: 2,
            outcome: "ok".into(),
            error_class: None,
            http_status: Some(200),
            error: None,
            elapsed_ms: 41_234,
            usage: Some(json!({"input_tokens": 10, "output_tokens": 20})),
            estimated_usd: Some(0.00065),
            usd_is_estimate: true,
            rates_as_of: "2026-10-09".into(),
        }
    }

    #[test]
    fn line_is_single_line_json_with_all_fields() {
        let line = sample().to_line();
        assert!(!line.contains('\n'));
        let v: Value = serde_json::from_str(&line).unwrap();
        for key in [
            "ts",
            "purpose",
            "endpoint",
            "model_requested",
            "model_used",
            "size",
            "requested_size",
            "quality",
            "attempt",
            "outcome",
            "error_class",
            "http_status",
            "error",
            "elapsed_ms",
            "usage",
            "estimated_usd",
            "usd_is_estimate",
            "rates_as_of",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(v["outcome"], "ok");
        assert_eq!(v["attempt"], 2);
        assert_eq!(v["usage"]["output_tokens"], 20);
        assert_eq!(v["error_class"], Value::Null);
        assert_eq!(v["usd_is_estimate"], true);
    }

    #[test]
    fn append_creates_dirs_and_appends_lines() {
        let dir = std::env::temp_dir().join(format!("iz-openai-ledger-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("image-calls.jsonl");
        append(&path, &sample());
        let mut second = sample();
        second.outcome = "error".into();
        second.error_class = Some("transient".into());
        append(&path, &second);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["error_class"], "transient");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_failure_does_not_panic() {
        // A path under a regular file cannot be created.
        let file = std::env::temp_dir().join(format!("iz-openai-ledger-file-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        append(&file.join("sub").join("ledger.jsonl"), &sample());
        let _ = std::fs::remove_file(&file);
    }
}
