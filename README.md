# The Looking Glass

*Have you ever wanted to step into a photograph?*

The Looking Glass is a native desktop app (Rust, macOS first, portable) that
lets you zoom into a photograph without end. Drag a box around anything and
the app shows you that region from closer. Keep going: continent, city,
street, a face, an eye, the cells in it, the chromosomes, and then, where
seeing stops making sense, atoms drawn as alien worlds you can zoom into all
over again. Zoom out and the photo becomes part of a wider world, all the way
to the observable universe and beyond. Tilt and turn the camera, take a step
forward through a door or a wall, and look around.

Everything past the real pixels is generated. Claude reasons about what should
be there, keeping a running dossier of the place, the time, the season and
what has already been drawn, and writes the prompt. OpenAI's image model
paints it. The result is as realistic as world knowledge allows, and openly
imaginary once the scale leaves the physical world.

## Status

In development. See [DESIGN.md](DESIGN.md) for the full design and the
decisions behind it.

## How it works

Every step is two calls:

1. **Describe.** Claude (Opus 5.5, through the headless `claude` CLI) sees
   the current frame, the history, and the requested move, and returns a
   structured scene spec: a title, the render prompt, a camera estimate, and
   an updated dossier.
2. **Paint.** One call to OpenAI's images API, built from that spec. Zooming
   in passes the crop and the current frame as references; zooming out places
   the current frame in the middle of a transparent canvas and asks the model
   to paint around it; camera moves are prompt-driven with the current frame
   as reference.

Worlds are trees of frames. Every view change is a node, so going back and
choosing a different box keeps both branches. A frame, once generated, is
permanent: zooming into the same spot again shows the same thing. Worlds are
saved on disk and can be reopened from the start screen.

Everything is safe for work by construction: the prompts insist on fully
clothed people and benign places regardless of what would be realistic.

## Requirements

- Rust (stable, 1.95 or newer).
- The `claude` CLI, logged in. Calls bill to that account.
- An OpenAI API key in a `.env` file at the repo root:

  ```
  OPENAI_API_KEY=sk-...
  ```

  `.env` is git-ignored. `chmod 600` it.

## Build and run

```sh
cargo run -p iz-app --release
```

Development without spending anything:

```sh
IZ_MOCK=1 cargo run -p iz-app
```

Mock mode fakes both services with placeholder frames and realistic delays.

Environment variables:

| Variable         | Meaning                                                  |
| ---------------- | -------------------------------------------------------- |
| `OPENAI_API_KEY` | OpenAI key (also read from `.env`).                      |
| `IZ_MOCK`        | `1` uses the mock services. No network, no cost.         |
| `IZ_CLAUDE_BIN`  | Path to the `claude` binary, if it is not on your PATH.  |
| `IZ_DATA_DIR`    | Overrides the data directory (worlds, settings, ledger). |

Data lives under `~/Library/Application Support/TheLookingGlass/` on macOS
(the platform data directory elsewhere): `worlds/<id>/` with every frame and
every prompt and reply, `settings.json`, and `image-calls.jsonl`, a ledger of
every image API call with its usage and estimated cost. The app never deletes
anything. Full prompts and replies also go to stdout.

## Controls

| Input             | Action                                      |
| ----------------- | ------------------------------------------- |
| Drag a rectangle  | Zoom in (the box snaps to the frame shape)  |
| Click an outline  | Re-enter a frame you zoomed into before     |
| Z                 | Zoom out                                    |
| Left / Right      | Turn the camera                             |
| Up / Down         | Tilt like an aircraft stick: Up noses down, Down pulls up to the sky |
| W                 | Step forward, through a wall or door if one fills the view |
| Home              | Look straight down, facing north            |
| Backspace         | Back to the previous frame                  |
| Shift+Backspace   | Forward again                               |
| R                 | Regenerate this frame (asks first if it has branches, then discards them) |
| X                 | Cancel the call in progress                 |
| F                 | Fullscreen                                  |
| Esc               | Help and settings                           |

## Layout

| Crate              | What it is                                                  |
| ------------------ | ----------------------------------------------------------- |
| `crates/iz-types`  | Shared types, traits, settings, image ops. The contract.    |
| `crates/iz-claude` | The describer: prompts and the headless `claude` wrapper.   |
| `crates/iz-openai` | The painter: raw HTTP to the OpenAI images API, the ledger. |
| `crates/iz-world`  | World persistence, the pipeline, the mock services.         |
| `crates/iz-app`    | The egui user interface and the binary.                     |
