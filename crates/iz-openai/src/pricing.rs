//! Rough cost estimates from the `usage` object. These are list prices copied
//! by hand and will drift; every figure derived here is labelled an estimate.

use serde_json::Value;

/// When the table below was read from https://developers.openai.com/api/docs/pricing
/// (standard tier, Images API).
pub const RATES_AS_OF: &str = "2026-10-09";

/// USD per one million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub text_in: f64,
    pub image_in: f64,
    pub image_out: f64,
    /// Not listed for most image models; text output is normally zero.
    pub text_out: Option<f64>,
}

const RATES: [(&str, Rates); 6] = [
    ("gpt-image-2.5-sunburst", Rates { text_in: 5.0, image_in: 8.0, image_out: 30.0, text_out: None }),
    ("gpt-image-2.5-flare", Rates { text_in: 5.0, image_in: 8.0, image_out: 30.0, text_out: None }),
    ("gpt-image-2", Rates { text_in: 5.0, image_in: 8.0, image_out: 30.0, text_out: None }),
    ("gpt-image-1.5", Rates { text_in: 5.0, image_in: 8.0, image_out: 32.0, text_out: Some(10.0) }),
    ("gpt-image-1-mini", Rates { text_in: 2.0, image_in: 2.5, image_out: 8.0, text_out: None }),
    ("gpt-image-1", Rates { text_in: 5.0, image_in: 10.0, image_out: 40.0, text_out: None }),
];

/// Rates for a model or a dated snapshot of it (`gpt-image-2-2026-04-21`).
pub fn rates_for(model: &str) -> Option<Rates> {
    RATES.iter().find_map(|(base, rates)| {
        model
            .strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
            .then_some(*rates)
    })
}

fn tokens(v: &Value, path: &[&str]) -> Option<f64> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_f64()
}

/// Estimated USD for one call. None when the model is not in the table or the
/// response carried no usage.
pub fn estimate_usd(model: &str, usage: Option<&Value>) -> Option<f64> {
    let rates = rates_for(model)?;
    let usage = usage.filter(|u| u.is_object())?;
    let input_total = tokens(usage, &["input_tokens"]).unwrap_or(0.0);
    let output_total = tokens(usage, &["output_tokens"]).unwrap_or(0.0);
    let (text_in, image_in) = match usage.get("input_tokens_details") {
        Some(d) => (
            tokens(d, &["text_tokens"]).unwrap_or(0.0),
            tokens(d, &["image_tokens"]).unwrap_or(0.0),
        ),
        None => (input_total, 0.0),
    };
    let (text_out, image_out) = match usage.get("output_tokens_details") {
        Some(d) => (
            tokens(d, &["text_tokens"]).unwrap_or(0.0),
            tokens(d, &["image_tokens"]).unwrap_or(0.0),
        ),
        None => (0.0, output_total),
    };
    let usd = (text_in * rates.text_in
        + image_in * rates.image_in
        + image_out * rates.image_out
        + text_out * rates.text_out.unwrap_or(rates.text_in))
        / 1_000_000.0;
    Some((usd * 1e6).round() / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rates_match_snapshots_but_not_siblings() {
        assert_eq!(rates_for("gpt-image-2").unwrap().image_out, 30.0);
        assert_eq!(rates_for("gpt-image-2-2026-04-21").unwrap().image_out, 30.0);
        assert_eq!(rates_for("gpt-image-2.5-flare-2026-09-08").unwrap().image_in, 8.0);
        assert_eq!(rates_for("gpt-image-1").unwrap().image_out, 40.0);
        assert_eq!(rates_for("gpt-image-1.5").unwrap().image_out, 32.0);
        assert_eq!(rates_for("gpt-image-1-mini").unwrap().image_out, 8.0);
        assert!(rates_for("gpt-image-20").is_none());
        assert!(rates_for("dall-e-3").is_none());
    }

    #[test]
    fn estimate_from_usage() {
        let usage = json!({
            "input_tokens": 1050,
            "input_tokens_details": {"text_tokens": 50, "image_tokens": 1000},
            "output_tokens": 4160,
            "total_tokens": 5210
        });
        // 50*5 + 1000*8 + 4160*30 = 250 + 8000 + 124800 = 133050 per million
        let usd = estimate_usd("gpt-image-2", Some(&usage)).unwrap();
        assert!((usd - 0.13305).abs() < 1e-9, "{usd}");
        let gen = json!({"input_tokens": 20, "output_tokens": 272, "total_tokens": 292});
        let usd = estimate_usd("gpt-image-1", Some(&gen)).unwrap();
        assert!((usd - (20.0 * 5.0 + 272.0 * 40.0) / 1e6).abs() < 1e-9, "{usd}");
        assert!(estimate_usd("unknown-model", Some(&usage)).is_none());
        assert!(estimate_usd("gpt-image-2", None).is_none());
        assert!(estimate_usd("gpt-image-2", Some(&Value::Null)).is_none());
    }
}
