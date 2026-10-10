# The Looking Glass — design

*Have you ever wanted to step into a photograph?*

A native desktop app (Rust, macOS first, portable) that lets you zoom "infinitely"
into and out of a photograph. Every step past the real pixels is generated:
Claude (via the headless `claude` CLI, Opus 5.5) reasons about what should be
there and writes the render prompt; OpenAI's image API paints it.

## The experience

- Open screen: pick a photo from disk, drag one onto the window, "I'm feeling
  lucky" (Claude picks a real, named place on Earth at random and the image
  model renders a photo of it), or open a previously saved world.
- Main screen: mostly the image. Drag a rectangle to zoom in. The rectangle
  snaps to the frame's aspect ratio.
- Zoom passes through regimes: cosmic, planetary, geographic, street, human,
  eye, cellular, molecular (chromosomes, DNA; artificial lighting allowed),
  then a deliberate break into the imaginary: atoms are drawn as alien planets,
  each random, with their own landscape, vegetation, maybe cities, and the whole
  loop recurses. Scale between chromosome and atom may be compressed.
- Zoom out widens the view. The current frame sits in the middle of the new
  one. Past the Earth: solar system, galaxy, observable universe. Past the
  observable universe the view resolves into something imaginary (a surprise
  the user asked not to be told; see the Claude prompt guidance) and the
  recursion continues upward.
- Camera: tilt (0 = straight down, 90 = horizon), heading (0 = north), and
  "move forward one meter" which is exactly one meter along the view direction
  and passes through doors and walls. One meter is for ground level; zoom is
  how you move quickly at altitude.
- Realism: as realistic as the world knowledge allows. Known landmarks look
  like themselves; unknown houses look typical for the region. People are
  plausible, never real.
- Everything is safe for work. Everyone is fully clothed regardless of
  setting, even where that is unrealistic. No sexual content, no racy
  establishments, no gore, no drugs. If the natural continuation would be a
  racy place, Claude substitutes a benign one.

## Pipeline: two calls per step, no verify pass

1. Claude (describe): sees the current frame (and crop/parent where relevant),
   the prior scene spec, the ancestry, and the transition. Returns a structured
   SceneSpec: title, render prompt, camera estimate, canvas mode (ZoomIn only),
   and an updated "dossier" (location, time of day, season, established facts).
2. OpenAI (paint): one images call built from the spec.
   - ZoomIn, canvas mode Crop: first image is the upscaled crop; references:
     current frame. For small jumps (under about 3x).
   - ZoomIn, canvas mode Reference: no canvas; references: crop + current frame.
   - ZoomOut: canvas = current frame centered at 1/factor on a transparent
     canvas, mask = same alpha (transparent = paint here). Pipeline forces this.
   - Look / Forward: no canvas; references: current frame (+ parent).
   - Root / Lucky: Root analyses the photo, no paint. Lucky paints from a
     prompt with no references (generations endpoint).
3. If the image call is refused, Claude rewrites the prompt once in more
   generic, safer terms. If refused again, the app shows "can't go further
   here". There is no retry-until-perfect loop and no verify pass. Stitching
   is a reasonable prompt-based effort, not a guarantee.

## World model

- A world is a tree of nodes (one DAG edge: see ZoomOut below). Every view
  change is a node: ZoomIn{rect}, ZoomOut{factor}, Look{tilt,heading}
  (absolute), Forward{meters} (negative = backward). Backspace goes back to the
  parent; a redo key goes forward again.
- Discrete transitions (ZoomOut, Look, Forward) are repeatable exactly, so the
  engine looks up an existing child with the same transition before
  generating. ZoomIn is reused only by clicking an existing outline.
- ZoomOut back-link: when a ZoomOut node Z is created from N with factor f,
  N is also recorded as a child of Z with the centered 1/f rect, so zooming
  back into the center returns the original frame. N's parent is unchanged.
- Previously drawn rectangles are shown as faint outlines on their parent
  frame; clicking inside one re-enters that branch.
- Regenerate replaces the current node's image (new describe + paint) and
  discards its descendants. The UI asks for confirmation first when the node
  has children; a leaf regenerates straight away.
- Determinism comes from the cache: a committed frame is permanent. The image
  API has no seed.
- Worlds persist under the app's data dir (`~/Library/Application Support/
  TheLookingGlass/worlds/<id>/`): world.json, images/, calls/ (prompt + reply
  records), thumb.png. The app never deletes anything.

## UI contract

- Frames are 1536x1024 (settings). Window letterboxes.
- Status bar: which service is being called, what for, elapsed time on that
  call; breadcrumb of regime and scale. Full prompts and replies also go to
  stdout with image data replaced by file paths.
- ESC: help + settings (image quality, image model, Claude model, frame size,
  zoom-out factor). Settings apply to frames generated from then on.
- Keys: drag = zoom in; Left/Right = heading ∓45°; Up = tilt toward the horizon, Down = toward straight down, 30° steps;
  W/S = forward/backward one meter; Z = zoom out; Home = straight down, north;
  Backspace = back; Shift+Backspace = redo; R = regenerate; X = cancel;
  F = fullscreen; ESC = help/settings.
- Input is blocked while a generation runs, except ESC and X.

## Services

- Claude: `claude -p` with `--model claude-opus-5-5`, `--output-format json`,
  `--json-schema`, a system prompt, and the Read tool pre-approved so it can
  look at PNGs in the world directory. The child's environment has
  `CLAUDECODE` and `CLAUDE_CODE_*` removed so it can run from inside another
  session. Binary found via a login shell or `IZ_CLAUDE_BIN`. Bills to the
  user's Claude Code account.
- OpenAI: raw HTTP (reqwest) to /v1/images/edits and /v1/images/generations,
  key from `.env` (`OPENAI_API_KEY`). Model default gpt-image-2.5-sunburst with
  fallbacks (flare, gpt-image-2, 1.5, 1) if the account lacks access. Quality
  default high. Every call appended to a JSONL ledger with usage.
- Mock mode (`IZ_MOCK=1`): fake Claude and fake painter, no network, no cost.
  Default for development.

## Spend policy

Mock mode for development. Real calls during the build: about ten low-quality
images in total across the OpenAI test and the integration run, each in the
ledger; a handful of Claude calls.

## Workspace

- `crates/iz-types`: shared types, traits, settings, image ops, logging. The
  contract every other crate builds against.
- `crates/iz-claude`: Describer implementation over the claude CLI + prompts.
- `crates/iz-openai`: Painter implementation over the OpenAI images API.
- `crates/iz-world`: world persistence, pipeline/engine, mocks.
- `crates/iz-app`: eframe/egui UI, binary.

Rules for implementers: no `cargo add`, no `cargo update`, no edits to
Cargo.toml or Cargo.lock; additive changes to iz-types are allowed only with a
passing `cargo check --workspace` and a note in the report; breaking changes
mean stop and report.
