//! Routing and per-model request parameters. Pure functions of the request
//! and the model name, so they are tested without a network.

use crate::http::{Body, EncodedInputs};
use crate::models::{api_quality, api_size, family};
use iz_types::PaintRequest;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// POST /images/generations, JSON body, prompt only.
    Generations,
    /// POST /images/edits, multipart, one or more input images.
    Edits,
}

impl Endpoint {
    /// Path segment under `/images/`; also the ledger's `endpoint` value.
    pub fn name(self) -> &'static str {
        match self {
            Endpoint::Generations => "generations",
            Endpoint::Edits => "edits",
        }
    }
}

/// The routing rule documented on [`PaintRequest`]: any input image (canvas
/// or reference) means the edits endpoint; none means generations.
pub fn route(req: &PaintRequest) -> Endpoint {
    if req.canvas.is_some() || !req.references.is_empty() {
        Endpoint::Edits
    } else {
        Endpoint::Generations
    }
}

/// Everything sent for one model; retries reuse it.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub endpoint: Endpoint,
    pub model: String,
    /// Size sent to the API; the result is resized to the requested size.
    pub api_w: u32,
    pub api_h: u32,
    pub quality: &'static str,
    /// Sent on the edits endpoint for gpt-image-1 and 1.5 only.
    pub input_fidelity: Option<&'static str>,
    /// Human-readable adjustments made for this model (logged as notes).
    pub notes: Vec<String>,
}

impl Plan {
    pub fn new(req: &PaintRequest, model: &str) -> Plan {
        let fam = family(model);
        let endpoint = route(req);
        let (api_w, api_h) = api_size(fam, req.width, req.height);
        let quality = api_quality(fam, req.quality);
        let mut notes = Vec::new();
        if (api_w, api_h) != (req.width, req.height) {
            notes.push(format!(
                "{model} cannot paint {}x{}; asking for {api_w}x{api_h} and resizing the result to {}x{}",
                req.width, req.height, req.width, req.height
            ));
        }
        if quality != req.quality.as_str() {
            notes.push(format!("{model} does not accept quality {}; using {quality}", req.quality.as_str()));
        }
        let input_fidelity = match endpoint {
            Endpoint::Edits => fam.input_fidelity(),
            Endpoint::Generations => None,
        };
        Plan { endpoint, model: model.to_string(), api_w, api_h, quality, input_fidelity, notes }
    }

    pub fn api_size(&self) -> (u32, u32) {
        (self.api_w, self.api_h)
    }

    pub fn size(&self) -> String {
        format!("{}x{}", self.api_w, self.api_h)
    }

    /// Form fields for the edits endpoint (images and mask are added from
    /// [`EncodedInputs`]).
    pub fn edit_fields(&self, req: &PaintRequest) -> Vec<(&'static str, String)> {
        let mut fields = vec![
            ("model", self.model.clone()),
            ("prompt", req.prompt.clone()),
            ("size", self.size()),
            ("quality", self.quality.to_string()),
            ("output_format", "png".to_string()),
            ("n", "1".to_string()),
        ];
        if let Some(fidelity) = self.input_fidelity {
            fields.push(("input_fidelity", fidelity.to_string()));
        }
        if req.canvas.is_some() {
            // A transparent canvas must not invite a transparent result.
            fields.push(("background", "opaque".to_string()));
        }
        fields
    }

    /// The request body. `inputs` must be Some for the edits endpoint.
    pub fn body(&self, req: &PaintRequest, inputs: Option<Arc<EncodedInputs>>) -> Body {
        match self.endpoint {
            Endpoint::Generations => Body::Json(json!({
                "model": self.model,
                "prompt": req.prompt,
                "size": self.size(),
                "quality": self.quality,
                "output_format": "png",
                "n": 1,
            })),
            Endpoint::Edits => Body::Multipart {
                fields: self.edit_fields(req),
                inputs: inputs.unwrap_or_default(),
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use iz_types::Quality;

    pub(crate) fn request(canvas: bool, mask: bool, refs: usize) -> PaintRequest {
        let img = || RgbaImage::from_pixel(1536, 1024, Rgba([10, 20, 30, 255]));
        PaintRequest {
            model: "gpt-image-2.5-sunburst".into(),
            prompt: "a lighthouse".into(),
            width: 1536,
            height: 1024,
            quality: Quality::High,
            canvas: canvas.then(img),
            mask: mask.then(img),
            references: (0..refs).map(|_| img()).collect(),
            purpose: "test".into(),
        }
    }

    #[test]
    fn routing() {
        assert_eq!(route(&request(false, false, 0)), Endpoint::Generations);
        assert_eq!(route(&request(false, false, 2)), Endpoint::Edits);
        assert_eq!(route(&request(true, false, 0)), Endpoint::Edits);
        assert_eq!(route(&request(true, true, 1)), Endpoint::Edits);
    }

    #[test]
    fn generation_body() {
        let req = request(false, false, 0);
        let plan = Plan::new(&req, "gpt-image-2");
        let Body::Json(v) = plan.body(&req, None) else { panic!("expected JSON") };
        assert_eq!(v["model"], "gpt-image-2");
        assert_eq!(v["size"], "1536x1024");
        assert_eq!(v["quality"], "high");
        assert_eq!(v["output_format"], "png");
        assert_eq!(v["n"], 1);
        assert!(v.get("input_fidelity").is_none());
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn edit_fields_per_model() {
        let req = request(true, true, 1);
        let f = Plan::new(&req, "gpt-image-2.5-sunburst").edit_fields(&req);
        assert!(f.iter().all(|(k, _)| *k != "input_fidelity"));
        assert!(f.contains(&("background", "opaque".into())));
        let f = Plan::new(&req, "gpt-image-1.5").edit_fields(&req);
        assert!(f.contains(&("input_fidelity", "high".into())));
        let f = Plan::new(&request(false, false, 2), "gpt-image-1").edit_fields(&req);
        assert!(f.contains(&("input_fidelity", "high".into())));
        // Generations never carry input_fidelity, even on gpt-image-1.
        assert_eq!(Plan::new(&request(false, false, 0), "gpt-image-1").input_fidelity, None);
    }

    #[test]
    fn standard_only_models_snap_and_note() {
        let mut req = request(false, false, 0);
        req.width = 1920;
        req.height = 1088;
        req.quality = Quality::Max;
        let p = Plan::new(&req, "gpt-image-1.5");
        assert_eq!(p.api_size(), (1536, 1024));
        assert_eq!(p.quality, "high");
        assert_eq!(p.notes.len(), 2, "{:?}", p.notes);
        let p = Plan::new(&req, "gpt-image-2.5-flare");
        assert_eq!(p.api_size(), (1920, 1088));
        assert_eq!(p.quality, "max");
        assert!(p.notes.is_empty());
    }
}
