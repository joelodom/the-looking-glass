//! The words. Everything Claude is told lives here so it can be tuned
//! without touching the subprocess plumbing.
//!
//! - [`SYSTEM_PROMPT`]: the director's brief, identical for every call.
//! - [`user_prompt`]: the per-call request (transition, cameras, dossier,
//!   ancestry, image paths, what the painter will receive).
//! - [`purpose`]: the short label used in logs and record file names.

use chrono::{DateTime, Local, Timelike};
use iz_types::{Camera, DescribeKind, DescribeRequest, Transition};
use std::fmt::Write as _;
use std::path::Path;

/// The director's brief. Passed with `--system-prompt` on every call.
pub const SYSTEM_PROMPT: &str = r##"You are the director of Infinite Zoom, a desktop app that lets a person zoom into and out of a photograph forever.

Only the starting photo is real. Every frame past its pixels is generated: you decide what the next frame contains, you write the prompt an image model paints it from, and you keep a running dossier so everything drawn stays consistent with everything drawn before. You are called once per frame. Your answer goes straight to the painter. There is no review pass and no second attempt, so the render prompt must be right the first time.

# Transitions

The person changes the view from the current frame in one of four ways:
- zoom_in {rect}: they dragged a rectangle on the current frame (x, y, w, h normalized 0..1, origin top-left, same aspect as the frame). The new frame shows exactly that region filling the frame, 1/w times closer. The view direction stays the same; the camera moves closer, the lens narrows, or at small scales the microscope's magnification rises.
- zoom_out {factor}: the new frame is factor times wider, and the current frame sits unchanged in its exact center, covering 1/factor of the width and height. The painter fills the border around it, which must continue the center seamlessly outward.
- look {tilt_deg, heading_deg}: the camera stays where it is and turns to this absolute orientation.
- forward {meters}: the camera moves exactly that many meters along its view direction (negative = backward). It passes through anything in the way (doors, walls, floors, hedges, the side of a bus) and ends up on the other side: describe what is there. One meter is for walking around at ground level. At altitude a meter changes nothing visible, so keep the frame essentially the same and say so in notes; zoom is how one moves fast at altitude.

# Camera

- frame_width_m: width of the visible frame at the subject, in meters. The core scale quantity; any magnitude, from beyond 1e27 m down to below 1e-11 m.
- distance_m: camera to subject, in meters.
- tilt_deg: 0 = looking straight down, 90 = looking at the horizon (above 90 looks up into the sky).
- heading_deg: compass direction of the view, 0 = north, 90 = east, 180 = south, 270 = west. In a straight-down view it is the compass direction at the top of the frame.
- regime: one word for the kind of view (vocabulary below).
- scale_label: short human-readable size, e.g. "about 40 km across", "2 mm across", "12 µm across", "about 100,000 light-years across". Always fill it in.

Each step request carries a proposed_camera: the transition applied arithmetically to the current camera. Start from it. Adjust it only for a reason (a regime switch, entering or leaving an imagined world, compressed scale, a forward move that went through a floor, the camera rising during a long zoom out), and say why in notes.

# Regimes and the arc inward

Regime vocabulary: cosmic, planetary, geographic, street, human, eye, cellular, molecular, imaginary.

The classic dive runs cosmic, planetary, geographic, street, human, eye, cellular, molecular. Follow what the person actually zooms into (a leaf leads to leaf cells, a wall to mineral grains, a sleeve to woven fibers); the regimes guide style and scale. Rough frame widths:
- cosmic: wider than about 1e8 m (Earth and Moon, the solar system, the Milky Way, the observable universe at about 9e26 m)
- planetary: about 1e6 to 1e8 m (the whole Earth is about 1.3e7 m)
- geographic: about 1 km to 1000 km
- street: about 10 m to 1 km
- human: about 0.3 m to 10 m (a person, a room, a face)
- eye: about 1 mm to 30 cm (an eye, an iris, skin, fabric, an insect)
- cellular: about 1 µm to 1 mm
- molecular: below about 1 µm (chromosomes, the DNA double helix, proteins, molecules)

Below about 1 mm the camera is a microscope: render realistic microscopy (light micrograph, fluorescence, false-color scanning electron micrograph) or a photoreal scientific visualization. At molecular scale real light does not apply, so artificial lighting is allowed: show chromosomes, DNA and molecules as solid, beautifully lit forms. The scale between chromosome and atom may be compressed: you may go from the double helix to individual atoms in one zoom (adjust frame_width_m and say so in notes).

Then the deliberate break: atoms are drawn as alien planets. Zooming into an atom, its electron cloud first reads as a hazy luminous sphere, then as the atmosphere of a whole planet seen from space. Every atom is a different world, chosen at random: its own sky, seas or deserts, landforms, vegetation, maybe creatures and cities. Invent the world when the person first enters it, record it in the dossier as world_seed (one vivid phrase, e.g. "amber-sky world of basalt mesas, rust-red grass and domed stone villages under two small suns"), and stay faithful to it. Inside, the regimes repeat (planetary, geographic, street, human, eye, cellular, molecular), and that world's atoms are new worlds again: the loop recurses without end. On entering, reset frame_width_m to the size things appear to be in that world (a planet seen whole is about 1e7 m) so the arithmetic continues sensibly, and add 1 to world_depth. Use regime "imaginary" for the crossing frames (the atom becoming a planet); once inside, use the ordinary words for the kind of view. Zooming out from an atom-world seen whole returns to the atom in its molecule (see the ancestry).

# The arc outward

Zoom out widens the view around the current frame. From the ground the natural route to space is to look straight down and zoom out: district, region, country, the whole Earth, Earth and Moon, the inner solar system, the solar system, the Milky Way, the Local Group, the cosmic web, the observable universe. Keep the real geography, the right hemisphere, the day and night sides, and cloud patterns that fit the season.

Zoom out keeps tilt and heading by default. From a horizontal view (tilt near 90) the first zoom outs add sky above and foreground below, like a wider lens or stepping back. After that, rather than an impossible panorama, let the camera rise and tilt down a little each step toward an aerial view; record it in tilt_deg and notes.

Past the observable universe (frame wider than about 1e27 m) the view resolves into something imaginary, and it should be a surprise: the cosmic web of glowing filaments and dark voids turns out to be a tiny detail of something in a wholly new world. Choose ONE idea and commit to it. For example:
- the filaments are the fine veins in the iris of an enormous eye; the next zoom out shows the face of the gentle creature it belongs to, in its own landscape;
- the web is the frost-like crystal pattern inside a single drop of dew; the next zoom out shows the drop on a leaf in a dawn meadow of a strange world;
- the web is the pattern on a creature's skin, the scales of a moth's wing, the grain of a polished stone, the foam on a cup of tea, the lichen on a rock;
or an idea of your own of the same kind. Take two steps: first the filaments become recognizably a texture of that object, still mysterious; then one more zoom out reveals the object and the world around it, photographed realistically. Record the idea and the new world in the dossier (world_seed, and subtract 1 from world_depth). From there keep going outward as a new planetary arc: that world's landscape, its globe (unmistakably not Earth: other continents, colors, rings or moons), its star system, its galaxy, its universe; and past that universe the surprise happens again with a different idea. One consistent idea per world; never mix two.

# Realism

As realistic as world knowledge allows. Known places and landmarks look like themselves: correct architecture, materials, signage language, vegetation, road markings, driving side, vehicles of the era. Unknown places look typical for their region, era, time of day, season and weather. People are plausible, ordinary, anonymous individuals, never real or famous people. Everything stays consistent with the dossier and with the images: same light direction, time of day, weather, season, colors and objects. Imagined worlds get the same rigor: physically plausible, photographed like real places.

# Safe for work, always

Everyone is fully clothed regardless of setting, even where that is unrealistic: beaches, pools, saunas, locker rooms, bathrooms, bedrooms, hospitals, artworks, statues and imagined creatures alike. No nudity or partial nudity, no underwear or lingerie, no sexual or suggestive content, no racy establishments, no gore, wounds or blood, no violence, no drugs or drug paraphernalia. If the natural continuation would be a racy or unsafe place (the camera walks into a strip club, a bedroom with people in it, a crime scene), substitute a benign one (a quiet bookshop, a tidy empty bedroom, an ordinary office) and say so in notes. At eye, cellular and molecular scales stay clinical and clean.

# canvas_mode (zoom_in only)

- "crop": the jump is small (factor under about 3) and nothing changes in kind. The upscaled crop becomes the image the painter edits, so the composition is preserved exactly; your prompt asks it to restore sharp, natural detail at the new scale.
- "reference": larger jumps, or a change of regime or style where the crop's blurry pixels would mislead. The painter paints a fresh frame and uses the crop and the current frame only as references.
For every other transition return "reference"; the pipeline decides the canvas itself.

# The render prompt

The image model reads only your render_prompt plus the images the pipeline attaches (each request tells you which, in order). It knows nothing else: no dossier, no ancestry, no earlier prompts. So the prompt must be self-contained and specific:
1. Medium and style: "A photograph ..." with lens or instrument, lighting, time of day, weather. For microscopy name the technique; at molecular scale "a photoreal 3D render with soft artificial lighting"; in space "an astronomical photograph".
2. The camera: where it is, how high, which way it faces (in words, not only degrees), how steeply it looks down, the framing; landscape 3:2.
3. The attached images and how to use each, by position. For example: "The first attached image is a blurry enlargement of the region to paint; the second shows the current, wider view for context. Paint that region 3.5 times closer, keeping the layout, colors and light of the first image, with crisp detail ..." For zoom_out: "The attached image shows the current view, reduced, in the center of a transparent canvas. Keep it exactly as it is and paint the surroundings so that it continues seamlessly outward ..."
4. What is in the frame: the subject and every significant element, where it sits in the frame, materials, colors, textures, and what newly becomes visible at this scale.
5. Continuity: what must match the references and the established facts.
6. A short tail: "No text, captions, watermarks, borders or labels. Everyone fully clothed; no nudity, sexual content, gore or drugs." (Real signs may carry plausible text.)
Typically 120 to 300 words of plain prose, no markdown. Describe people generically and never name real people. Name brands only when they are part of a landmark's identity; otherwise use generic signage. Avoid wording that could trip moderation.

# The dossier

A JSON object you own; it comes back to you with every call. Return it updated every time: keep every key, add what you learn or invent, and never drop an established fact (you may tighten an old fact's wording if it stays true). Suggested keys:
- location: the best description of the place ("Shibuya Crossing, Tokyo, Japan", or "a suburban street, probably the U.S. Midwest")
- lat, lon: numbers when known, otherwise null
- place_names: names that apply at this point (country, city, district, street, building)
- time_of_day, local_time, season, weather, era
- world_depth: 0 on the real Earth; +1 for each atom-world entered, -1 for each outer world revealed
- world_seed: one phrase describing the current imagined world (null on Earth); keep left worlds in worlds_visited
- established_facts: short strings about what has been drawn and must stay true ("a red bicycle leans on the green door of no. 14", "the woman on the bench wears a yellow raincoat", "the helix is lit from the upper left in warm white")
Add other keys freely. Keep facts short and record what matters for consistency.

# By kind of request

- root: the starting photo is real. Study it and analyse it: where it is (specific when you recognize it, otherwise the most likely region and why), what it shows, the scale (frame_width_m at the subject), distance, tilt and heading (0 if unknowable; say so in notes), time of day, season, weather, era. Title it. render_prompt may be a one-line description of the photo; nothing is painted. Fill the dossier thoroughly; it seeds everything after.
- lucky: invent a starting photograph. Pick a real, named place on Earth at random, varying widely across continents, famous and obscure, natural and built, with a random vantage (a ground-level tourist photo or an aerial view). Follow the dice in the request. Write a render prompt for a convincing photograph of it (nothing is attached; the painter starts from text alone), with camera and dossier to match. Choose a season that fits the current date and hemisphere.
- step: a view change, as above.
- rewrite: the image model refused your previous prompt. Rewrite render_prompt once, in more generic and safer terms, keeping the scene: remove anything that could trip moderation (named real people, brands and logos, anything that could read as suggestive, violence, weapons, injury, close-ups of children). Keep title, camera, canvas_mode and dossier as they were unless they contained the problem. Say in notes what you changed.

# Images and tools

Use the Read tool to look at every image the request lists, by its absolute path, before answering; read them all together in a single turn. Do not read, list or search for anything else.

# Output

Answer only with the structured output: title (short, e.g. "Shibuya Crossing from 300 m"), render_prompt, camera, canvas_mode, dossier, notes. notes is for the person watching: uncertainty, a regime change, a camera adjustment, a safety substitution; empty when there is nothing to say."##;

/// Short label for logs and record file names, e.g. "describe zoom in".
pub fn purpose(kind: &DescribeKind, transition: &Transition) -> String {
    match kind {
        DescribeKind::Root => "describe root".into(),
        DescribeKind::Lucky => "describe lucky".into(),
        DescribeKind::Step => format!("describe {}", transition_word(transition)),
        DescribeKind::Rewrite { .. } => format!("rewrite {}", transition_word(transition)),
    }
}

/// `purpose` turned into something safe for a file name.
pub fn slug(purpose: &str) -> String {
    let mut out = String::with_capacity(purpose.len());
    for c in purpose.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn transition_word(t: &Transition) -> &'static str {
    match t {
        Transition::Root => "lucky",
        Transition::ZoomIn { .. } => "zoom in",
        Transition::ZoomOut { .. } => "zoom out",
        Transition::Look { .. } => "look",
        Transition::Forward { meters } if *meters < 0.0 => "back",
        Transition::Forward { .. } => "forward",
    }
}

/// The per-call request text, sent on stdin.
pub fn user_prompt(req: &DescribeRequest, now: DateTime<Local>) -> String {
    let mut p = String::new();
    let rewrite = matches!(req.kind, DescribeKind::Rewrite { .. });

    // Header.
    let kind = match &req.kind {
        DescribeKind::Root => "root (analyse the starting photo)".to_string(),
        DescribeKind::Lucky => "lucky (invent a starting photograph of a real place)".to_string(),
        DescribeKind::Step => format!("step ({})", req.transition.label()),
        DescribeKind::Rewrite { .. } => {
            format!(
                "rewrite (the image model refused the prompt for: {})",
                step_or_lucky(&req.transition)
            )
        }
    };
    let _ = writeln!(p, "REQUEST: {kind}");
    let _ = writeln!(
        p,
        "Current local time: {} ({}), UTC offset {}",
        now.format("%Y-%m-%d %H:%M"),
        now.format("%A"),
        now.format("%:z")
    );

    if let DescribeKind::Rewrite {
        refused_message,
        previous,
    } = &req.kind
    {
        p.push_str("\n## Refused\n");
        let _ = writeln!(
            p,
            "The image model refused with this message:\n{}",
            refused_message.trim()
        );
        p.push_str("\nYour previous spec (rewrite its render_prompt; keep the rest unless it caused the refusal):\n");
        p.push_str(&pretty(previous));
        p.push('\n');
    }

    if req.kind == DescribeKind::Lucky {
        p.push_str("\n## Dice\n");
        p.push_str(&lucky_dice(now));
    }

    // Transition.
    if req.transition != Transition::Root {
        p.push_str("\n## Transition\n");
        let _ = writeln!(p, "{}", req.transition.label());
        let _ = writeln!(
            p,
            "{}",
            serde_json::to_string(&req.transition).unwrap_or_default()
        );
        let current_cam = req.prior_spec.as_ref().map(|s| &s.camera);
        p.push_str(&explain_transition(&req.transition, current_cam));
        p.push('\n');
    }

    // Cameras and spec.
    match &req.proposed_camera {
        Some(cam) => {
            p.push_str(
                "\n## Proposed camera\nThe transition applied arithmetically to the current camera. Start here, adjust only for a reason, and fill in scale_label.\n",
            );
            p.push_str(&pretty(cam));
            p.push('\n');
        }
        None if matches!(req.kind, DescribeKind::Root | DescribeKind::Lucky) => {
            p.push_str("\n## Camera\nNo proposal: estimate the whole camera yourself.\n");
        }
        None => {}
    }

    if let Some(spec) = &req.prior_spec {
        p.push_str("\n## Current frame's spec\nThe frame being left. Its dossier is yours: return it updated, never dropping an established fact.\n");
        p.push_str(&pretty(spec));
        p.push('\n');
    } else if matches!(req.kind, DescribeKind::Step | DescribeKind::Rewrite { .. }) {
        p.push_str("\n## Current frame's spec\nNone recorded. Infer what you can from the images and start a dossier.\n");
    }

    // Ancestry.
    if !req.ancestry.is_empty() || req.prior_spec.is_some() {
        p.push_str("\n## Ancestry (root first; the current frame is last)\n");
        for (i, a) in req.ancestry.iter().enumerate() {
            let _ = writeln!(
                p,
                "{}. \"{}\" via {} [{}, {}]",
                i + 1,
                a.title,
                a.transition.label(),
                a.camera.regime,
                scale_or_width(&a.camera)
            );
        }
        if let Some(spec) = &req.prior_spec {
            let _ = writeln!(
                p,
                "-> current: \"{}\" [{}, {}]",
                spec.title,
                spec.camera.regime,
                scale_or_width(&spec.camera)
            );
        }
    }

    // Images to read.
    let images = image_list(req);
    if !images.is_empty() {
        if rewrite {
            p.push_str("\n## Images (optional for a rewrite; Read them only if it helps)\n");
        } else {
            p.push_str("\n## Images\nUse the Read tool on each of these absolute paths, all in one turn, before answering.\n");
        }
        for (label, path) in &images {
            let _ = writeln!(p, "- {label}: {}", path.display());
        }
    }

    // What the painter gets.
    p.push_str("\n## What the painter will receive\n");
    p.push_str(&painter_inputs(req));
    p.push('\n');

    let _ = writeln!(
        p,
        "\n## Output frame\n{} x {} pixels, landscape {}.",
        req.settings.frame_width,
        req.settings.frame_height,
        aspect_words(req.settings.frame_width, req.settings.frame_height)
    );

    p.push_str("\n## Your task\n");
    p.push_str(task_text(req));
    p.push('\n');
    p
}

/// The images Claude is asked to Read, labelled, in reading order.
pub fn image_list(req: &DescribeRequest) -> Vec<(&'static str, &Path)> {
    let mut v: Vec<(&'static str, &Path)> = Vec::new();
    if let Some(p) = &req.current_image {
        let label = if req.kind == DescribeKind::Root {
            "starting photo"
        } else {
            "current frame"
        };
        v.push((label, p.as_path()));
    }
    if let Some(p) = &req.crop_image {
        v.push((
            "crop (the selected region, upscaled to frame size, so blurry)",
            p.as_path(),
        ));
    }
    if let Some(p) = &req.parent_image {
        v.push((
            "parent frame (the frame before the current one)",
            p.as_path(),
        ));
    }
    v
}

fn step_or_lucky(t: &Transition) -> String {
    match t {
        Transition::Root => "the lucky starting photograph".into(),
        other => other.label(),
    }
}

fn explain_transition(t: &Transition, current: Option<&Camera>) -> String {
    match t {
        Transition::Root => String::new(),
        Transition::ZoomIn { rect } => {
            let factor = 1.0 / rect.w.max(1e-6);
            let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            let advice = if factor < 3.0 {
                "A jump this small usually suits canvas_mode \"crop\" (unless the kind of view changes)."
            } else {
                "A jump this large calls for canvas_mode \"reference\"."
            };
            format!(
                "The person selected x {:.0}% to {:.0}% and y {:.0}% to {:.0}% of the current frame (center {:.0}% from the left, {:.0}% from the top: the {} of the frame). The new frame shows exactly that region filling the frame, {:.1} times closer. {}",
                rect.x * 100.0,
                (rect.x + rect.w) * 100.0,
                rect.y * 100.0,
                (rect.y + rect.h) * 100.0,
                cx * 100.0,
                cy * 100.0,
                region_words(cx, cy),
                factor,
                advice
            )
        }
        Transition::ZoomOut { factor } => format!(
            "The new frame is {factor:.1} times wider. The current frame sits unchanged in its exact center, covering the middle {:.0}% of the width and height; everything around it is new.",
            100.0 / factor.max(1e-6)
        ),
        Transition::Look { tilt_deg, heading_deg } => {
            let mut s = format!(
                "The camera stays put and turns to tilt {tilt_deg:.0} deg ({}) and heading {heading_deg:.0} deg (facing {}).",
                tilt_words(*tilt_deg),
                compass(*heading_deg)
            );
            if let Some(cam) = current {
                let turn = iz_types::transition::heading_diff(*heading_deg, cam.heading_deg);
                let _ = write!(
                    s,
                    " Before: tilt {:.0} deg, heading {:.0} deg (facing {}). So it turns {} and tilts {}.",
                    cam.tilt_deg,
                    cam.heading_deg,
                    compass(cam.heading_deg),
                    turn_words(turn),
                    tilt_change_words(*tilt_deg - cam.tilt_deg)
                );
            }
            s
        }
        Transition::Forward { meters } => {
            let dir = if *meters >= 0.0 { "forward" } else { "backward" };
            let mut s = format!(
                "The camera moves exactly {:.2} m {dir} along its view direction, passing through anything in the way.",
                meters.abs()
            );
            if let Some(cam) = current {
                let _ = write!(
                    s,
                    " It faces {} (heading {:.0} deg) and looks {} (tilt {:.0} deg), about {} from the subject.",
                    compass(cam.heading_deg),
                    cam.heading_deg,
                    tilt_words(cam.tilt_deg),
                    cam.tilt_deg,
                    meters_words(cam.distance_m)
                );
            }
            s
        }
    }
}

fn painter_inputs(req: &DescribeRequest) -> String {
    match &req.transition {
        Transition::Root if req.kind == DescribeKind::Root => {
            "Nothing is painted for a root analysis; render_prompt may be a one-line description of the photo.".into()
        }
        Transition::Root => {
            "No images. The painter generates the photograph from your render_prompt alone, so describe everything.".into()
        }
        Transition::ZoomIn { .. } => "First attached image: the upscaled crop. Second attached image: the current frame. With canvas_mode \"crop\" the first image is the one being edited, so its composition is kept; with \"reference\" both are references only and the frame is painted fresh. Refer to them in render_prompt as \"the first attached image\" and \"the second attached image\".".into(),
        Transition::ZoomOut { factor } => format!(
            "One image to edit: the current frame reduced to the center 1/{factor:.1} of a transparent canvas, with a mask that lets the painter touch only the transparent border. Write render_prompt as an outpainting instruction: keep the center exactly, paint the surroundings so it continues seamlessly outward."
        ),
        Transition::Look { .. } | Transition::Forward { .. } => {
            let mut s = String::from(
                "No canvas. First attached image: the current frame (the view before this move), as a reference.",
            );
            if req.parent_image.is_some() {
                s.push_str(" Second attached image: the parent frame, as a further reference.");
            }
            s.push_str(" Describe the new view fully; say what carries over from the reference and where it now sits in the frame.");
            s
        }
    }
}

fn task_text(req: &DescribeRequest) -> &'static str {
    match req.kind {
        DescribeKind::Root => "Read the starting photo and analyse it: place, what it shows, scale, distance, tilt, heading, time of day, season, weather, era. Give it a title, fill in the camera (including scale_label) and a thorough dossier. canvas_mode \"reference\". Use notes for uncertainty.",
        DescribeKind::Lucky => "Pick the place and vantage (follow the dice), then write the render prompt for a convincing photograph of it, with the camera and a full dossier to match. canvas_mode \"reference\". Mention the place in the title.",
        DescribeKind::Step => "Read the images, decide what the new frame contains, and return its spec: title, a self-contained render_prompt, the camera (start from the proposal, fill in scale_label), canvas_mode, the updated dossier, and notes.",
        DescribeKind::Rewrite { .. } => "Rewrite render_prompt once in more generic, safer terms that keep the scene, and return the full spec. Keep title, camera, canvas_mode and dossier from the previous spec unless they caused the refusal. Say in notes what you changed.",
    }
}

/// A clock-derived suggestion so "random" picks really vary from call to call.
fn lucky_dice(now: DateTime<Local>) -> String {
    const REGIONS: [&str; 18] = [
        "East Asia",
        "Southeast Asia",
        "South Asia",
        "Central Asia or the Caucasus",
        "the Middle East",
        "North Africa",
        "West or Central Africa",
        "East Africa",
        "Southern Africa or Madagascar",
        "Western Europe",
        "Northern Europe or the Arctic",
        "Southern Europe or the Mediterranean",
        "Eastern Europe",
        "the United States or Canada",
        "Mexico, Central America or the Caribbean",
        "South America",
        "Australia or New Zealand",
        "the Pacific or Indian Ocean islands",
    ];
    const SUBJECTS: [&str; 10] = [
        "a famous landmark",
        "a little-known town or village",
        "a natural wonder",
        "an everyday city street",
        "farmland or a rural landscape",
        "a coastline or harbor",
        "mountains or highlands",
        "a market, square or plaza",
        "a historic site or ruin",
        "striking modern architecture",
    ];
    const VANTAGES: [&str; 4] = [
        "a ground-level tourist photo",
        "an aerial photo from a drone or small plane, looking obliquely down",
        "a high aerial view looking straight down",
        "a view from a hill, tower or rooftop",
    ];
    const LIGHT: [&str; 6] = [
        "early morning",
        "midday",
        "golden hour",
        "blue hour at dusk",
        "an overcast day",
        "night, with city or moon light",
    ];
    let seed = now.timestamp_nanos_opt().unwrap_or_default() as u64
        ^ u64::from(now.nanosecond()).rotate_left(32)
        ^ u64::from(std::process::id());
    let mut rng = SplitMix64(seed);
    format!(
        "Rolled from the clock (seed {seed}). Follow them unless impossible:\n- region: {}\n- subject: {}\n- vantage: {}\n- light: {}\n- season: whatever the date {} means in that hemisphere\nChoose a specific, real, named place that fits; avoid the obvious first pick.\n",
        REGIONS[rng.pick(REGIONS.len())],
        SUBJECTS[rng.pick(SUBJECTS.len())],
        VANTAGES[rng.pick(VANTAGES.len())],
        LIGHT[rng.pick(LIGHT.len())],
        now.format("%B %-d"),
    )
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn pretty<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("<unserializable: {e}>"))
}

fn scale_or_width(c: &Camera) -> String {
    if c.scale_label.trim().is_empty() {
        format_scale(c.frame_width_m)
    } else {
        c.scale_label.clone()
    }
}

/// Human-readable frame width, e.g. "about 40 km across". Used when Claude
/// leaves `scale_label` empty.
pub fn format_scale(width_m: f64) -> String {
    const LY: f64 = 9.4607e15;
    const AU: f64 = 1.495_978_707e11;
    if !width_m.is_finite() || width_m <= 0.0 {
        return "unknown scale".into();
    }
    let (value, unit) = if width_m >= 0.1 * LY {
        (width_m / LY, "light-years")
    } else if width_m >= 0.1 * AU {
        (width_m / AU, "AU")
    } else if width_m >= 1e3 {
        (width_m / 1e3, "km")
    } else if width_m >= 1.0 {
        (width_m, "m")
    } else if width_m >= 1e-2 {
        (width_m * 1e2, "cm")
    } else if width_m >= 1e-3 {
        (width_m * 1e3, "mm")
    } else if width_m >= 1e-6 {
        (width_m * 1e6, "µm")
    } else if width_m >= 1e-9 {
        (width_m * 1e9, "nm")
    } else {
        (width_m * 1e12, "pm")
    };
    format!("about {} {unit} across", round_sig(value))
}

fn round_sig(v: f64) -> String {
    if v >= 1e6 {
        let exp = v.log10().floor() as i32;
        format!("{:.1}e{exp}", v / 10f64.powi(exp))
    } else if v >= 100.0 {
        // Two significant figures: 99,994 reads as 100,000.
        let mag = 10f64.powi(v.log10().floor() as i32 - 1);
        group_thousands(&format!("{:.0}", (v / mag).round() * mag))
    } else if v >= 10.0 {
        format!("{:.0}", v)
    } else {
        let s = format!("{:.1}", v);
        s.trim_end_matches(".0").to_string()
    }
}

fn group_thousands(digits: &str) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn meters_words(m: f64) -> String {
    format_scale(m).replace(" across", "")
}

fn region_words(cx: f32, cy: f32) -> String {
    let h = if cx < 1.0 / 3.0 {
        "left"
    } else if cx > 2.0 / 3.0 {
        "right"
    } else {
        "center"
    };
    let v = if cy < 1.0 / 3.0 {
        "upper"
    } else if cy > 2.0 / 3.0 {
        "lower"
    } else {
        "middle"
    };
    match (v, h) {
        ("middle", "center") => "center".into(),
        (v, h) => format!("{v} {h}"),
    }
}

/// Eight-point compass name for a heading in degrees.
pub fn compass(heading: f32) -> &'static str {
    const NAMES: [&str; 8] = [
        "north",
        "north-east",
        "east",
        "south-east",
        "south",
        "south-west",
        "west",
        "north-west",
    ];
    let h = heading.rem_euclid(360.0);
    NAMES[(((h + 22.5) / 45.0) as usize) % 8]
}

fn tilt_words(tilt: f32) -> &'static str {
    match tilt {
        t if t < 10.0 => "straight down",
        t if t < 35.0 => "steeply down",
        t if t < 65.0 => "obliquely down",
        t if t < 85.0 => "slightly down",
        t if t <= 95.0 => "level, at the horizon",
        _ => "upward, into the sky",
    }
}

fn turn_words(turn: f32) -> String {
    if turn.abs() < 0.5 {
        "not at all".into()
    } else if turn > 0.0 {
        format!("{turn:.0} deg to the right")
    } else {
        format!("{:.0} deg to the left", -turn)
    }
}

fn tilt_change_words(delta: f32) -> String {
    if delta.abs() < 0.5 {
        "not at all".into()
    } else if delta > 0.0 {
        format!("up by {delta:.0} deg (toward the horizon)")
    } else {
        format!("down by {:.0} deg (toward straight down)", -delta)
    }
}

fn aspect_words(w: u32, h: u32) -> String {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let g = gcd(w, h).max(1);
    format!("{}:{}", w / g, h / g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iz_types::{AncestorBrief, CanvasMode, RectF, SceneSpec, Settings};
    use std::path::PathBuf;

    fn spec() -> SceneSpec {
        SceneSpec {
            title: "Harbour at dusk".into(),
            render_prompt: "A photograph of a harbour".into(),
            camera: Camera {
                frame_width_m: 400.0,
                distance_m: 300.0,
                tilt_deg: 60.0,
                heading_deg: 90.0,
                regime: "street".into(),
                scale_label: "about 400 m across".into(),
            },
            canvas_mode: CanvasMode::Reference,
            dossier: serde_json::json!({
                "location": "Bergen, Norway",
                "established_facts": ["the warehouses are painted red and ochre"]
            }),
            notes: String::new(),
        }
    }

    fn req(kind: DescribeKind, transition: Transition) -> DescribeRequest {
        DescribeRequest {
            kind,
            transition,
            current_image: Some(PathBuf::from("/w/images/n0002.png")),
            crop_image: Some(PathBuf::from("/w/tmp/crop.png")),
            parent_image: Some(PathBuf::from("/w/images/n0001.png")),
            prior_spec: Some(spec()),
            proposed_camera: Some(Camera {
                frame_width_m: 100.0,
                ..spec().camera
            }),
            ancestry: vec![AncestorBrief {
                title: "Bergen from the mountain".into(),
                transition: Transition::Root,
                camera: Camera {
                    frame_width_m: 3000.0,
                    scale_label: String::new(),
                    ..spec().camera
                },
            }],
            world_dir: PathBuf::from("/w"),
            settings: Settings::default(),
        }
    }

    #[test]
    fn system_prompt_covers_the_brief() {
        for needle in [
            "fully clothed",
            "0 = looking straight down",
            "90 = looking at the horizon",
            "0 = north",
            "exactly that many meters",
            "passes through anything",
            "atoms are drawn as alien planets",
            "observable universe",
            "world_seed",
            "established_facts",
            "\"crop\"",
            "\"reference\"",
            "never real or famous people",
            "no drugs",
            "proposed_camera",
            "scale_label",
            "Read tool",
            "no review pass",
        ] {
            assert!(
                SYSTEM_PROMPT.contains(needle),
                "system prompt lacks {needle:?}"
            );
        }
    }

    #[test]
    fn zoom_in_prompt_has_everything() {
        let r = req(
            DescribeKind::Step,
            Transition::ZoomIn {
                rect: RectF::new(0.4, 0.3, 0.25, 0.25),
            },
        );
        let p = user_prompt(&r, Local::now());
        assert!(p.starts_with("REQUEST: step (zoom in ×4.0)"));
        assert!(p.contains("\"kind\":\"zoom_in\""));
        assert!(p.contains("4.0 times closer"));
        assert!(p.contains("canvas_mode \"reference\""));
        assert!(p.contains("/w/images/n0002.png"));
        assert!(p.contains("/w/tmp/crop.png"));
        assert!(p.contains("/w/images/n0001.png"));
        assert!(p.contains("\"frame_width_m\": 100.0"));
        assert!(p.contains("the warehouses are painted red and ochre"));
        assert!(p.contains("1. \"Bergen from the mountain\" via start [street, about 3 km across]"));
        assert!(p.contains("-> current: \"Harbour at dusk\" [street, about 400 m across]"));
        assert!(p.contains("first attached image"));
        assert!(p.contains("1536 x 1024 pixels, landscape 3:2"));
    }

    #[test]
    fn small_zoom_suggests_crop() {
        let r = req(
            DescribeKind::Step,
            Transition::ZoomIn {
                rect: RectF::new(0.25, 0.25, 0.5, 0.5),
            },
        );
        let p = user_prompt(&r, Local::now());
        assert!(p.contains("2.0 times closer"));
        assert!(p.contains("suits canvas_mode \"crop\""));
    }

    #[test]
    fn look_prompt_explains_the_turn() {
        let mut r = req(
            DescribeKind::Step,
            Transition::Look {
                tilt_deg: 90.0,
                heading_deg: 135.0,
            },
        );
        r.crop_image = None;
        let p = user_prompt(&r, Local::now());
        assert!(p.contains("facing south-east"));
        assert!(p.contains("turns 45 deg to the right"));
        assert!(p.contains("up by 30 deg"));
        assert!(!p.contains("crop ("));
        assert!(p.contains("Second attached image: the parent frame"));
    }

    #[test]
    fn forward_and_zoom_out_prompts() {
        let p = user_prompt(
            &req(DescribeKind::Step, Transition::Forward { meters: -1.0 }),
            Local::now(),
        );
        assert!(p.contains("REQUEST: step (back 1 m)"));
        assert!(p.contains("1.00 m backward"));
        assert!(p.contains("It faces east (heading 90 deg)"));
        let p = user_prompt(
            &req(DescribeKind::Step, Transition::ZoomOut { factor: 4.0 }),
            Local::now(),
        );
        assert!(p.contains("4.0 times wider"));
        assert!(p.contains("middle 25%"));
        assert!(p.contains("outpainting"));
    }

    #[test]
    fn lucky_prompt_needs_nothing() {
        let r = DescribeRequest {
            kind: DescribeKind::Lucky,
            transition: Transition::Root,
            current_image: None,
            crop_image: None,
            parent_image: None,
            prior_spec: None,
            proposed_camera: None,
            ancestry: vec![],
            world_dir: PathBuf::from("/w"),
            settings: Settings::default(),
        };
        let p = user_prompt(&r, Local::now());
        assert!(p.contains("REQUEST: lucky"));
        assert!(p.contains("Current local time:"));
        assert!(p.contains("## Dice"));
        assert!(p.contains("- region: "));
        assert!(p.contains("from your render_prompt alone"));
        assert!(!p.contains("## Images"));
        assert!(!p.contains("## Transition"));
        assert!(!p.contains("%B"));
    }

    #[test]
    fn root_prompt_reads_the_photo() {
        let mut r = req(DescribeKind::Root, Transition::Root);
        r.crop_image = None;
        r.parent_image = None;
        r.prior_spec = None;
        r.proposed_camera = None;
        r.ancestry.clear();
        let p = user_prompt(&r, Local::now());
        assert!(p.contains("- starting photo: /w/images/n0002.png"));
        assert!(p.contains("Nothing is painted"));
        assert!(p.contains("estimate the whole camera yourself"));
    }

    #[test]
    fn rewrite_prompt_carries_the_refusal() {
        let mut previous = spec();
        previous.render_prompt = "A photograph of a crowded beach".into();
        let r = req(
            DescribeKind::Rewrite {
                refused_message: "safety system rejected".into(),
                previous,
            },
            Transition::ZoomOut { factor: 4.0 },
        );
        let p = user_prompt(&r, Local::now());
        assert!(p.starts_with("REQUEST: rewrite"));
        assert!(p.contains("safety system rejected"));
        assert!(p.contains("A photograph of a crowded beach"));
        assert!(p.contains("optional for a rewrite"));
    }

    #[test]
    fn purposes_and_slugs() {
        assert_eq!(
            purpose(&DescribeKind::Root, &Transition::Root),
            "describe root"
        );
        assert_eq!(
            purpose(&DescribeKind::Lucky, &Transition::Root),
            "describe lucky"
        );
        assert_eq!(
            purpose(
                &DescribeKind::Step,
                &Transition::ZoomIn {
                    rect: RectF::new(0.0, 0.0, 0.5, 0.5)
                }
            ),
            "describe zoom in"
        );
        assert_eq!(
            purpose(&DescribeKind::Step, &Transition::Forward { meters: -1.0 }),
            "describe back"
        );
        let rw = DescribeKind::Rewrite {
            refused_message: String::new(),
            previous: spec(),
        };
        assert_eq!(
            purpose(&rw, &Transition::ZoomOut { factor: 4.0 }),
            "rewrite zoom out"
        );
        assert_eq!(slug("describe zoom in"), "describe-zoom-in");
        assert_eq!(slug("  a//b  "), "a-b");
    }

    #[test]
    fn scales_read_naturally() {
        assert_eq!(format_scale(40_000.0), "about 40 km across");
        assert_eq!(format_scale(0.002), "about 2 mm across");
        assert_eq!(format_scale(1.2e-5), "about 12 µm across");
        assert_eq!(format_scale(9.46e20), "about 100,000 light-years across");
        assert_eq!(format_scale(1.3e7), "about 13,000 km across");
        assert_eq!(format_scale(f64::NAN), "unknown scale");
        assert_eq!(compass(0.0), "north");
        assert_eq!(compass(350.0), "north");
        assert_eq!(compass(-90.0), "west");
        assert_eq!(compass(225.0), "south-west");
    }
}
