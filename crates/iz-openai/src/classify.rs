//! Turning an HTTP status plus the API's error body into a decision: refuse,
//! fall back to another model, retry, or give up.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Moderation / content policy. Not retried; the pipeline rewrites.
    Refused,
    /// The model does not exist or the account cannot use it. Try the next.
    ModelUnavailable,
    /// Rate limit, server error, network trouble. Retried with backoff.
    Transient,
    /// Anything else, including auth failures and exhausted quota.
    Fatal,
}

impl ErrorClass {
    /// Name used in the ledger.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Refused => "refused",
            ErrorClass::ModelUnavailable => "model_unavailable",
            ErrorClass::Transient => "transient",
            ErrorClass::Fatal => "fatal",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub class: ErrorClass,
    pub message: String,
    pub status: Option<u16>,
}

/// The `error` object of an OpenAI error body, as far as it parses.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ApiError {
    pub message: String,
    pub code: String,
    pub kind: String,
    pub param: String,
}

impl ApiError {
    pub fn parse(body: &str) -> ApiError {
        let Ok(v) = serde_json::from_str::<Value>(body) else {
            return ApiError { message: excerpt(body, 300), ..Default::default() };
        };
        let e = v.get("error").unwrap_or(&v);
        let field = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let message = match field("message") {
            m if m.is_empty() => excerpt(body, 300),
            m => m,
        };
        ApiError { message, code: field("code"), kind: field("type"), param: field("param") }
    }
}

/// First `max` characters of `s`, single-line, with an ellipsis when cut.
pub fn excerpt(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

const MODERATION_CODES: [&str; 4] =
    ["moderation_blocked", "content_policy_violation", "content_filter", "safety_violation"];
const MODERATION_PHRASES: [&str; 5] = [
    "safety system",
    "not allowed by our safety",
    "content policy",
    "rejected as a result of our safety",
    "safety guidelines",
];
const MODEL_MISSING_PHRASES: [&str; 6] = [
    "does not have access",
    "invalid model",
    "model_not_found",
    "must be verified to use the model",
    "not allowed to sample from this model",
    "unknown model",
];

/// Classify a non-success HTTP response from the images API.
pub fn classify(status: u16, body: &str) -> Failure {
    let e = ApiError::parse(body);
    let lower = e.message.to_lowercase();
    let code = e.code.to_lowercase();
    let kind = e.kind.to_lowercase();
    let mk = |class: ErrorClass, message: String| Failure { class, message, status: Some(status) };

    // Quota before the generic 429 rule: retrying cannot fix it.
    if code == "insufficient_quota"
        || kind == "insufficient_quota"
        || code == "billing_hard_limit_reached"
        || code == "billing_not_active"
    {
        return mk(
            ErrorClass::Fatal,
            format!(
                "OpenAI quota or billing limit reached ({}): {}. Add credit or raise the limit at \
                 https://platform.openai.com/settings/organization/billing",
                if code.is_empty() { &kind } else { &code },
                e.message
            ),
        );
    }

    // Model missing or not allowed for this account, before the auth rule:
    // "Project does not have access to model" and "organization must be
    // verified" arrive as 403.
    let model_missing = status == 404
        || code == "model_not_found"
        || (status == 400 && e.param == "model")
        || (lower.contains("model") && MODEL_MISSING_PHRASES.iter().any(|p| lower.contains(p)))
        || (lower.contains("model") && lower.contains("does not exist"));
    if model_missing && status != 401 && status != 429 && status < 500 {
        return mk(ErrorClass::ModelUnavailable, e.message);
    }

    if status == 401 || status == 403 {
        return mk(ErrorClass::Fatal, format!("OpenAI rejected the API key ({status}): {}", e.message));
    }

    let moderation = MODERATION_CODES.contains(&code.as_str())
        || (kind == "image_generation_user_error" && code.contains("moderation"))
        || MODERATION_PHRASES.iter().any(|p| lower.contains(p));
    if (400..500).contains(&status) && status != 408 && status != 409 && status != 429 && moderation {
        return mk(ErrorClass::Refused, e.message);
    }

    if matches!(status, 408 | 409 | 429) || status >= 500 {
        return mk(ErrorClass::Transient, format!("HTTP {status}: {}", e.message));
    }

    mk(ErrorClass::Fatal, format!("OpenAI error {status}: {}", e.message))
}

/// Classify a failure that never produced an HTTP status.
pub fn classify_transport(err: &reqwest::Error) -> Failure {
    let class = if err.is_builder() { ErrorClass::Fatal } else { ErrorClass::Transient };
    let what = if err.is_timeout() {
        "timed out"
    } else if err.is_connect() {
        "could not connect"
    } else if err.is_body() || err.is_decode() {
        "connection dropped while reading the reply"
    } else {
        "request failed"
    };
    Failure { class, message: format!("{what}: {err}"), status: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(status: u16, body: &str) -> ErrorClass {
        classify(status, body).class
    }

    #[test]
    fn moderation_is_refused() {
        let body = r#"{"error":{"message":"Your request was rejected by the safety system. If you believe this is an error, contact us at help.openai.com and include the request ID req_123.","type":"image_generation_user_error","param":null,"code":"moderation_blocked","moderation_details":{"moderation_stage":"input","categories":["violence"]}}}"#;
        let f = classify(400, body);
        assert_eq!(f.class, ErrorClass::Refused);
        assert!(f.message.contains("safety system"));
        let body = r#"{"error":{"message":"Your request was rejected as a result of our safety system. Image descriptions generated from your prompt may contain text that is not allowed by our safety system.","type":"invalid_request_error","param":null,"code":"content_policy_violation"}}"#;
        assert_eq!(class(400, body), ErrorClass::Refused);
        let body = r#"{"error":{"message":"This request has been blocked by our content filters.","type":"invalid_request_error","code":"content_filter"}}"#;
        assert_eq!(class(400, body), ErrorClass::Refused);
    }

    #[test]
    fn model_access_falls_back() {
        let body = r#"{"error":{"message":"The model `gpt-image-2.5-sunburst` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#;
        assert_eq!(class(404, body), ErrorClass::ModelUnavailable);
        let body = r#"{"error":{"message":"Project `proj_abc` does not have access to model `gpt-image-2`","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#;
        assert_eq!(class(403, body), ErrorClass::ModelUnavailable);
        let body = r#"{"error":{"message":"Your organization must be verified to use the model `gpt-image-1`. Please go to: https://platform.openai.com/settings/organization/general and click on Verify Organization.","type":"invalid_request_error","param":null,"code":null}}"#;
        assert_eq!(class(403, body), ErrorClass::ModelUnavailable);
        let body = r#"{"error":{"message":"Invalid value: 'gpt-image-9'. Supported values are: 'gpt-image-1', 'gpt-image-1.5' and 'gpt-image-2'.","type":"invalid_request_error","param":"model","code":"invalid_value"}}"#;
        assert_eq!(class(400, body), ErrorClass::ModelUnavailable);
        let body = r#"{"error":{"message":"Invalid model specified.","type":"invalid_request_error"}}"#;
        assert_eq!(class(400, body), ErrorClass::ModelUnavailable);
    }

    #[test]
    fn auth_is_fatal() {
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-abc***xyz. You can find your API key at https://platform.openai.com/account/api-keys.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#;
        let f = classify(401, body);
        assert_eq!(f.class, ErrorClass::Fatal);
        assert!(f.message.contains("API key"));
        let body = r#"{"error":{"message":"Country, region, or territory not supported","type":"request_forbidden","param":null,"code":"unsupported_country_region_territory"}}"#;
        assert_eq!(class(403, body), ErrorClass::Fatal);
        let body = r#"{"error":{"message":"You are not allowed to sample from this model","type":"invalid_request_error"}}"#;
        assert_eq!(class(403, body), ErrorClass::ModelUnavailable);
    }

    #[test]
    fn rate_limits_and_server_errors_are_transient() {
        let body = r#"{"error":{"message":"Rate limit reached for gpt-image-2 in organization org-x on images per min (IPM): Limit 5, Used 5, Requested 1.","type":"requests","param":null,"code":"rate_limit_exceeded"}}"#;
        assert_eq!(class(429, body), ErrorClass::Transient);
        let body = r#"{"error":{"message":"The server had an error while processing your request. Sorry about that!","type":"server_error","param":null,"code":null}}"#;
        assert_eq!(class(500, body), ErrorClass::Transient);
        assert_eq!(class(502, "<html><body>Bad gateway</body></html>"), ErrorClass::Transient);
        assert_eq!(class(503, ""), ErrorClass::Transient);
    }

    #[test]
    fn insufficient_quota_is_fatal_not_transient() {
        let body = r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
        let f = classify(429, body);
        assert_eq!(f.class, ErrorClass::Fatal);
        assert!(f.message.contains("quota"), "{}", f.message);
        assert!(f.message.contains("billing"));
    }

    #[test]
    fn other_client_errors_are_fatal_with_message() {
        let body = r#"{"error":{"message":"Invalid size '1000x1000'. Width and height must be multiples of 16.","type":"invalid_request_error","param":"size","code":"invalid_value"}}"#;
        let f = classify(400, body);
        assert_eq!(f.class, ErrorClass::Fatal);
        assert!(f.message.contains("multiples of 16"));
        assert_eq!(f.status, Some(400));
        let f = classify(400, "not json at all");
        assert_eq!(f.class, ErrorClass::Fatal);
        assert!(f.message.contains("not json at all"));
    }

    #[test]
    fn api_error_parse_and_excerpt() {
        let e = ApiError::parse(r#"{"error":{"message":"m","type":"t","param":"p","code":"c"}}"#);
        assert_eq!(e, ApiError { message: "m".into(), code: "c".into(), kind: "t".into(), param: "p".into() });
        assert_eq!(excerpt("a\n b   c", 10), "a b c");
        assert_eq!(excerpt("abcdef", 3), "abc…");
    }
}
