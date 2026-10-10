//! Turning what `claude -p --output-format json` printed into a SceneSpec,
//! or into the right DescribeError.

use iz_types::{DescribeError, SceneSpec};
use serde_json::Value;

const SNIPPET: usize = 600;

/// Where the spec was found in the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The `structured_output` field that `--json-schema` fills.
    StructuredOutput,
    /// Fallback: JSON inside the `result` text.
    ResultText,
}

/// The result envelope: the single JSON object `--output-format json`
/// prints. Tolerates an array of messages (as with `--verbose`) and stray
/// lines before the JSON.
pub fn envelope(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    let parsed = serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .filter(|l| l.starts_with('{') || l.starts_with('['))
            .find_map(|l| serde_json::from_str::<Value>(l).ok())
    })?;
    match parsed {
        Value::Array(items) => items
            .into_iter()
            .rev()
            .find(|v| v.get("type").and_then(Value::as_str) == Some("result")),
        v @ Value::Object(_) => Some(v),
        _ => None,
    }
}

/// Interpret a finished run. `exit_ok` is the child's exit status.
pub fn interpret(
    stdout: &str,
    stderr: &str,
    exit_ok: bool,
) -> Result<(SceneSpec, Source), DescribeError> {
    let Some(env) = envelope(stdout) else {
        let text = format!("{stdout}\n{stderr}");
        return Err(if exit_ok {
            DescribeError::Parse(format!("claude printed no JSON reply: {}", snippet(&text)))
        } else {
            classify(&text, None)
        });
    };

    let is_error = env
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let subtype = env.get("subtype").and_then(Value::as_str);
    let success = !is_error && subtype.is_none_or(|s| s == "success");
    if !success {
        let mut text = error_text(&env);
        if !stderr.trim().is_empty() {
            text.push('\n');
            text.push_str(stderr);
        }
        return Err(classify(&text, subtype));
    }
    if !exit_ok && env.get("structured_output").is_none() && env.get("result").is_none() {
        return Err(classify(&format!("{stdout}\n{stderr}"), subtype));
    }
    spec_from_envelope(&env)
}

fn spec_from_envelope(env: &Value) -> Result<(SceneSpec, Source), DescribeError> {
    if let Some(so) = env.get("structured_output").filter(|v| !v.is_null()) {
        let value = match so {
            Value::String(s) => extract_json(s).ok_or_else(|| {
                DescribeError::Parse(format!("structured_output is not JSON: {}", snippet(s)))
            })?,
            other => other.clone(),
        };
        return to_spec(value).map(|s| (s, Source::StructuredOutput));
    }
    let text = env
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let value = extract_json(text).ok_or_else(|| {
        DescribeError::Parse(format!(
            "no structured_output and no JSON in the result text: {}",
            snippet(text)
        ))
    })?;
    to_spec(value).map(|s| (s, Source::ResultText))
}

fn to_spec(value: Value) -> Result<SceneSpec, DescribeError> {
    let shown = snippet(&value.to_string());
    serde_json::from_value(value).map_err(|e| DescribeError::Parse(format!("{e} in {shown}")))
}

/// JSON object inside free text: strips ``` fences and any prose around
/// the outermost braces.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    let (start, end) = (t.find('{')?, t.rfind('}')?);
    if end <= start {
        return None;
    }
    match serde_json::from_str::<Value>(&t[start..=end]) {
        Ok(v @ Value::Object(_)) => Some(v),
        _ => None,
    }
}

fn error_text(env: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(s) = env.get("subtype").and_then(Value::as_str) {
        parts.push(s.to_string());
    }
    if let Some(r) = env.get("result").and_then(Value::as_str) {
        parts.push(r.to_string());
    }
    if let Some(errs) = env.get("errors").filter(|v| !v.is_null()) {
        parts.push(errs.to_string());
    }
    if parts.is_empty() {
        parts.push(env.to_string());
    }
    parts.join(": ")
}

/// Map an error text to a DescribeError: rate limits, overload and network
/// trouble are transient; login, billing and model problems are fatal.
pub fn classify(text: &str, subtype: Option<&str>) -> DescribeError {
    let lower = text.to_lowercase();
    let shown = snippet(text);
    const TRANSIENT: [&str; 21] = [
        "rate limit",
        "rate_limit",
        "429",
        "too many requests",
        "overloaded",
        "529",
        "usage limit",
        "limit reached",
        "timed out",
        "timeout",
        "econnreset",
        "econnrefused",
        "enotfound",
        "socket hang up",
        "network",
        "internal server error",
        "service unavailable",
        "bad gateway",
        "api error: 500",
        "api error: 502",
        "api error: 503",
    ];
    const FATAL: [&str; 9] = [
        "not logged in",
        "/login",
        "invalid api key",
        "authentication",
        "unauthorized",
        "credit balance",
        "invalid model",
        "model not found",
        "not_found_error",
    ];
    if TRANSIENT.iter().any(|n| lower.contains(n)) {
        return DescribeError::Transient(format!("claude: {shown}"));
    }
    if FATAL.iter().any(|n| lower.contains(n)) {
        return DescribeError::Fatal(format!(
            "claude: {shown} (run `claude` in a terminal to check the login and model)"
        ));
    }
    match subtype {
        Some("error_max_turns") => {
            DescribeError::Transient(format!("claude ran out of turns before answering: {shown}"))
        }
        Some(s) if s.contains("structured_output") => DescribeError::Parse(format!(
            "claude could not produce the structured reply: {shown}"
        )),
        _ => DescribeError::Fatal(format!("claude failed: {shown}")),
    }
}

/// First `SNIPPET` characters of `s`, on one line.
pub fn snippet(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(SNIPPET) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iz_types::CanvasMode;

    const SPEC: &str = r#"{"title":"Blue over green","render_prompt":"A field under a sky","camera":{"frame_width_m":120.0,"distance_m":80.0,"tilt_deg":85.0,"heading_deg":0.0,"regime":"geographic","scale_label":"about 120 m across"},"canvas_mode":"reference","dossier":{"location":"unknown","established_facts":["sky above, grass below"]},"notes":"guessing"}"#;

    fn ok_envelope(field: &str) -> String {
        format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"duration_ms":21000,"num_turns":3,"result":"done","session_id":"x","total_cost_usd":0.12,{field}}}"#
        )
    }

    #[test]
    fn structured_output_field_wins() {
        let out = ok_envelope(&format!(r#""structured_output":{SPEC}"#));
        let (spec, src) = interpret(&out, "", true).unwrap();
        assert_eq!(src, Source::StructuredOutput);
        assert_eq!(spec.title, "Blue over green");
        assert_eq!(spec.canvas_mode, CanvasMode::Reference);
        assert_eq!(spec.camera.scale_label, "about 120 m across");
    }

    #[test]
    fn structured_output_as_string_is_accepted() {
        let quoted = serde_json::to_string(SPEC).unwrap();
        let out = ok_envelope(&format!(r#""structured_output":{quoted}"#));
        let (spec, _) = interpret(&out, "", true).unwrap();
        assert_eq!(spec.notes, "guessing");
    }

    #[test]
    fn falls_back_to_fenced_result_text() {
        let text = format!("Here you go:\n```json\n{SPEC}\n```");
        let out = serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false, "result": text
        })
        .to_string();
        let (spec, src) = interpret(&out, "", true).unwrap();
        assert_eq!(src, Source::ResultText);
        assert_eq!(spec.camera.frame_width_m, 120.0);
    }

    #[test]
    fn verbose_array_and_leading_noise() {
        let arr = format!(
            r#"[{{"type":"system","subtype":"init"}},{{"type":"assistant"}},{}]"#,
            ok_envelope(&format!(r#""structured_output":{SPEC}"#))
        );
        assert!(interpret(&arr, "", true).is_ok());
        let noisy = format!(
            "warning: something\n{}",
            ok_envelope(&format!(r#""structured_output":{SPEC}"#))
        );
        assert!(interpret(&noisy, "", true).is_ok());
    }

    #[test]
    fn rate_limit_is_transient() {
        let out = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"}}"}"#;
        assert!(matches!(
            interpret(out, "", false),
            Err(DescribeError::Transient(_))
        ));
        assert!(matches!(
            interpret("", "Error: Claude AI usage limit reached|1760000000", false),
            Err(DescribeError::Transient(_))
        ));
        let overloaded =
            r#"{"type":"result","is_error":true,"result":"API Error: 529 Overloaded"}"#;
        assert!(matches!(
            interpret(overloaded, "", false),
            Err(DescribeError::Transient(_))
        ));
    }

    #[test]
    fn login_trouble_is_fatal() {
        let out = r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}"#;
        match interpret(out, "", false) {
            Err(DescribeError::Fatal(m)) => assert!(m.contains("/login")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            interpret("", "error: unknown option '--zzz'", false),
            Err(DescribeError::Fatal(_))
        ));
    }

    #[test]
    fn max_turns_and_garbage() {
        let out = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":8}"#;
        assert!(matches!(
            interpret(out, "", false),
            Err(DescribeError::Transient(_))
        ));
        assert!(matches!(
            interpret("not json at all", "", true),
            Err(DescribeError::Parse(_))
        ));
        let wrong = ok_envelope(r#""structured_output":{"title":"only a title"}"#);
        assert!(matches!(
            interpret(&wrong, "", true),
            Err(DescribeError::Parse(_))
        ));
        let prose = r#"{"type":"result","subtype":"success","is_error":false,"result":"I could not see the image."}"#;
        assert!(matches!(
            interpret(prose, "", true),
            Err(DescribeError::Parse(_))
        ));
    }

    #[test]
    fn json_extraction() {
        assert!(extract_json("```\n{\"a\":1}\n```").is_some());
        assert!(extract_json("Sure! {\"a\": {\"b\": 2}} Hope that helps.").is_some());
        assert!(extract_json("[1,2]").is_none());
        assert!(extract_json("} {").is_none());
        let long = "x".repeat(2000);
        assert!(snippet(&long).chars().count() <= SNIPPET + 1);
    }
}
