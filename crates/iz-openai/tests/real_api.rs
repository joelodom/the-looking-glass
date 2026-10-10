//! Real calls to the OpenAI images API. Costs money: runs only with
//! `IZ_REAL_TESTS=1`. Makes exactly two image calls (one generation, one
//! edit), plus whatever zero-cost "model unavailable" replies the fallback
//! walk meets. Output PNGs go to `$IZ_TEST_OUT` (default: a temp dir).
//!
//! `IZ_REAL_TESTS=1 cargo test -p iz-openai --test real_api -- --nocapture`

use iz_openai::OpenAiPainter;
use iz_types::{imageops, CallContext, PaintRequest, Painter, Quality, Settings, StdoutLog};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Mean absolute RGB difference between two equally sized images.
fn mean_abs_diff(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    let total: u64 = a
        .pixels()
        .zip(b.pixels())
        .map(|(p, q)| (0..3).map(|i| (p[i] as i64 - q[i] as i64).unsigned_abs()).sum::<u64>())
        .sum();
    total as f64 / (a.width() as f64 * a.height() as f64 * 3.0)
}

#[test]
fn real_generate_then_extend() {
    if std::env::var(iz_types::env::REAL_TESTS).as_deref() != Ok("1") {
        eprintln!("skipping: set IZ_REAL_TESTS=1 to call the real API");
        return;
    }
    let out = std::env::var("IZ_TEST_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("iz-openai-real"));
    std::fs::create_dir_all(&out).unwrap();

    let painter = OpenAiPainter::new();
    let mut ctx = CallContext::new(Arc::new(StdoutLog));
    ctx.record_dir = Some(out.join("calls"));
    let model = Settings::default().image_model;

    // (a) Generation.
    let prompt = "a small red lighthouse on a rocky shore at noon, photograph";
    let req = PaintRequest {
        model: model.clone(),
        prompt: prompt.into(),
        width: 1024,
        height: 1024,
        quality: Quality::Low,
        canvas: None,
        mask: None,
        references: Vec::new(),
        purpose: "real test generate".into(),
    };
    // A rerun (say after a free failure in (b)) reuses (a) instead of
    // paying for another generation.
    let a_path = out.join("a-generate.png");
    let (a_image, a_model) = match imageops::load_rgba(&a_path) {
        Ok(img) => {
            println!("\nREAL (a) reusing {} (delete it to generate again)", a_path.display());
            (img, painter.working_model().unwrap_or(model.clone()))
        }
        Err(_) => {
            let t = Instant::now();
            let a = painter.paint(&req, &ctx).expect("generation");
            let a_secs = t.elapsed().as_secs_f64();
            imageops::save_png(&a.image, &a_path).unwrap();
            println!(
                "\nREAL (a) generation: requested {model}, used {}, {a_secs:.1} s, usage {:?}, est ${:?} -> {}",
                a.model_used,
                a.usage,
                a.estimated_usd,
                a_path.display()
            );
            (a.image, a.model_used)
        }
    };
    assert_eq!(a_image.dimensions(), (1024, 1024));

    // (b) Edit: the generated frame at half size in the middle of a
    // transparent canvas; paint the surroundings.
    let (canvas, mask, rect) = imageops::center_on_transparent(&a_image, 1024, 1024, 0.5);
    imageops::save_png(&canvas, &out.join("b-canvas.png")).unwrap();
    let req = PaintRequest {
        model: a_model,
        prompt: "Zoom out. The opaque center of this image is an existing photograph of a small red \
                 lighthouse on a rocky shore at noon. Keep that center exactly as it is and extend the \
                 scene outward on all sides into the transparent area: more rocky coastline, sea and sky, \
                 with continuous lighting, perspective and color. One seamless photograph."
            .into(),
        width: 1024,
        height: 1024,
        quality: Quality::Low,
        canvas: Some(canvas),
        mask: Some(mask),
        references: Vec::new(),
        purpose: "real test extend".into(),
    };
    let t = Instant::now();
    let b = painter.paint(&req, &ctx).expect("edit");
    let b_secs = t.elapsed().as_secs_f64();
    let b_path = out.join("b-extend.png");
    imageops::save_png(&b.image, &b_path).unwrap();
    assert_eq!(b.image.dimensions(), (1024, 1024));

    // How closely the center of (b) matches (a) scaled down.
    let center = imageops::crop_norm(&b.image, rect);
    let expected = imageops::resize_to(&a_image, center.width(), center.height());
    let diff = mean_abs_diff(&center, &expected);
    println!(
        "REAL (b) edit: used {}, {b_secs:.1} s, usage {:?}, est ${:?}, center mean abs diff {diff:.1}/255 -> {}",
        b.model_used,
        b.usage,
        b.estimated_usd,
        b_path.display()
    );
    println!("ledger: {}", iz_types::settings::paths::ledger_file().display());
}
