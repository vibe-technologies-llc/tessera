# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Tessera is a video editor for Linux, written in Rust with a GPUI interface and hardware-accelerated
decode, render and encode. It is a desktop application only: there is no CLI surface to design,
parse or keep stable.

## Architecture

A Cargo workspace under `crates/`. Dependencies point one way:
`tessera-timeline` ← `tessera-media`, `tessera-render` ← `tessera-ui` ← `tessera` (the binary).

- **`tessera-timeline`** is the pure project model (`Project`, `Timeline`, `Track`, `Clip`,
  `Asset`). Each `Asset` carries its probed `MediaInfo` (duration and streams), so nothing outside
  `tessera-media` has to call into FFmpeg to describe a clip. It has no IO and no GPU, and it
  depends only on `thiserror`. Time is `Time(i64)` in flicks (1/705 600 000 s). Every common frame
  rate, the NTSC 1000/1001 rates included, has an integral frame duration in flicks, so frame ↔ time
  conversion through `FrameRate` stays exact. Never store time as floating-point seconds;
  `as_seconds_f64` exists only for drawing. `Timecode` labels a time at a frame rate, using
  drop-frame (`;` before the frames) for the 30000/1001 and 60000/1001 rates. A `Track` keeps its
  clips sorted by start and refuses overlapping inserts. `Project::clip_for` builds the clip that
  would cover a whole asset at a start time on a track, checking the stream kind, the duration and
  overlaps without mutating, and `Project::place_clip` inserts it. Later video tracks sit on top of
  earlier ones, so `Timeline::top_video_clip_at` searches them from the last.
- **`tessera-media`** is the only crate allowed to touch FFmpeg (`ffmpeg-next`, bindgen against the
  system FFmpeg). It covers probing, hwaccel discovery and frame decode, and encode goes here.
  `VideoDecoder::frame_at` returns a shared packed BGRA `VideoFrame`: it decodes forward from the
  current position unless the stream index shows a keyframe past it, and keeps recent frames in a
  byte-bounded LRU cache keyed by the span each frame covers. FFmpeg types do not cross its public
  API, apart from the `FfmpegError` re-export. `VideoDecoder` is `Send` so it can move to a
  background task: ffmpeg-next leaves its scaler context `!Send`, but an `SwsContext` has no thread
  affinity and each one is owned by a single decoder, so `Scaler` implements `Send` by hand.
- **`tessera-render`** owns a `wgpu` Vulkan `Compositor` (device + queue) for compositing timeline
  frames. It isn't wired into the UI yet.
- **`tessera-ui`** holds the GPUI views. `Workspace` owns an `Entity<Project>`, and each panel
  (`MediaBin`, `Viewer`, `TimelinePanel`) gets a clone of it and re-renders through
  `cx.observe(&project, …)`. Mutate the project through the entity so every panel updates. Global
  actions and keybindings are registered in `tessera_ui::init`; actions that need the project are
  handled on the focused `Workspace`, whose key context (`WORKSPACE_CONTEXT`) scopes the
  single-key transport bindings. The `Workspace` also owns an `Entity<Playhead>` shared the same
  way: the playhead is UI state, not part of the project, and scrubbing snaps it to frame starts.
  The playhead also runs the transport: space plays and pauses, J/K/L shuttle (each press doubles
  the speed up to 8× in that direction) and the arrow keys step one frame. Playing anchors a wall
  clock at the start time and a ticker task moves the playhead to the frame start under
  `anchor + elapsed × speed`, so it never drifts, and stops on the timeline's last frame or at
  zero. Any seek or step pauses playback. The timeline's ruler is a `canvas` that paints its ticks
  and timecode labels and scrubs the playhead through window mouse listeners, so a drag keeps
  tracking outside the ruler.
  `MediaBin` owns importing (dialog and drops) and probes each file and decodes its thumbnail on a
  background task, reporting failures in the bin itself. Thumbnails are UI state kept in the bin by
  `AssetId`, not part of the project model. Bin rows drag a `DraggedAsset`: each timeline lane
  previews the drop as a ghost (red where it would overlap) through `on_drag_move`, and the drop
  places the clip at the previewed, frame-snapped start. The `Viewer` shows the top video clip's
  frame under the playhead, decoded at sequence size on a background task. It keeps one decoder
  per asset and runs one decode at a time, so while scrubbing only the newest request is decoded
  next, and it releases each replaced frame from the GPUI atlas with `Window::drop_image`.
- **`tessera`** sets up tracing (`RUST_LOG`, `info` by default), initialises the media backend and
  opens the main window.

GPUI 0.2.2 renders through blade and cannot share a `wgpu` device. Until that changes, composited
frames reach the viewer as a GPUI image read back to the CPU, not through a shared GPU texture.

## Commands

- Build: `cargo build`
- Run: `cargo run`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Test everything: `cargo test`
- One crate's tests: `cargo test -p tessera-timeline`
- Single test: `cargo test -p <crate> <module::tests::test_name> -- --exact`
- Format: `rust-formatter` (check only: `rust-formatter --check`). Never `cargo fmt` or `rustfmt`.

A wave counts as verified when `rust-formatter --check`, `cargo clippy --all-targets -- -D warnings`
and `cargo test` all pass.

Building needs the system FFmpeg development headers (the build runs bindgen, so it needs clang)
and a Vulkan driver.

## Conventions

- **Rust 2024, no MSRV.** Use the newest language and std features the current stable toolchain
  offers; do not add `rust-version` or hold back for older compilers.
- **Dependencies are fully pinned to their latest release.** Every entry uses an exact requirement
  (`= "x.y.z"`, or a `rev` for a git source) at the newest version available when it is added or
  bumped. Check the registry for the current version (`cargo search`, `cargo info`) rather than writing one
  from memory. Every version lives in root `[workspace.dependencies]`, and member crates use
  `name.workspace = true`. `rust-formatter` also formats the `Cargo.toml` files (dotted keys, and
  long inline tables wrapped), so let it decide the layout.
- **Imports are grouped std, external, crate**, with one merged `use` per crate. `rust-formatter`
  enforces this (`StdExternalCrate` grouping, `Crate` granularity), so write imports that way and
  let it settle the rest.
- **Large work is split into waves across conversations.** Each conversation lands a small,
  self-contained piece that builds and passes the checks above, rather than one sweeping change.
- **`docs/TODO.md` is the roadmap.** It is a list of `## entry` headings, each with `- item`
  bullets. Remove an item in the same change that completes it, and an entry once it has no items
  left.
