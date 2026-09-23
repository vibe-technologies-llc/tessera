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
  `Asset`). It has no IO and no GPU, and it depends only on `thiserror`. Time is `Time(i64)` in
  flicks (1/705 600 000 s). Every common frame rate, the NTSC 1000/1001 rates included, has an
  integral frame duration in flicks, so frame ↔ time conversion through `FrameRate` stays exact.
  Never store time as floating-point seconds; `as_seconds_f64` exists only for drawing. A `Track`
  keeps its clips sorted by start and refuses overlapping inserts.
- **`tessera-media`** is the only crate allowed to touch FFmpeg (`ffmpeg-next`, bindgen against the
  system FFmpeg). It covers probing and hwaccel discovery, and decode/encode goes here. FFmpeg types
  do not cross its public API, apart from the `FfmpegError` re-export.
- **`tessera-render`** owns a `wgpu` Vulkan `Compositor` (device + queue) for compositing timeline
  frames. It isn't wired into the UI yet.
- **`tessera-ui`** holds the GPUI views. `Workspace` owns an `Entity<Project>`, and each panel
  (`MediaBin`, `Viewer`, `TimelinePanel`) gets a clone of it and re-renders through
  `cx.observe(&project, …)`. Mutate the project through the entity so every panel updates.
  Global actions and keybindings are registered in `tessera_ui::init`.
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
