# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Tessera is a video editor for Linux, written in Rust with a GPUI interface and hardware-accelerated
decode, render and encode. It is a desktop application only: there is no CLI surface to design,
parse or keep stable.

## Architecture

A Cargo workspace under `crates/`. Dependencies point one way:
`tessera-timeline` ← `tessera-media`, `tessera-document`; `tessera-media` ← `tessera-audio`;
`tessera-render` stands alone; all of them ← `tessera-ui` ← `tessera` (the binary).

- **`tessera-timeline`** is the pure project model (`Project`, `Timeline`, `Track`, `Clip`,
  `Asset`). Each `Asset` carries its probed `MediaInfo` (duration and streams), so nothing outside
  `tessera-media` has to call into FFmpeg to describe a clip. It has no IO and no GPU, and it
  depends only on `thiserror`. Time is `Time(i64)` in flicks (1/705 600 000 s). A `FrameRate` may be
  any positive rational up to one frame per flick, kept in lowest terms. Every rate in
  `FrameRate::STANDARD`, the NTSC 1000/1001 rates included, has an integral frame duration in
  flicks. At other rates (44/1) frame `n` starts on the first flick at or after its exact start
  (`frame_to_time` takes the ceiling, `time_to_frame` the floor), so frames differ in length by at
  most one flick, `frame_duration` is the first frame's, and frame ↔ time still round-trips
  exactly. Probing reads a video's rate through `FrameRate::nearest`, which snaps an average rate
  within 0.05 % of a standard one (2997/100) onto it and keeps any other rate as it is. Never store time as floating-point seconds;
  `as_seconds_f64` exists only for drawing. `Time` arithmetic saturates at `Time::MIN` and
  `Time::MAX` instead of wrapping or panicking, and so do the frame, sample and rational
  conversions; `checked_add`, `checked_sub` and `TimeRange::checked_end` report an overflow instead.
  Nothing divides by zero: a `FrameRate` exists only through `FrameRate::new`, which refuses a zero
  numerator or denominator and a rate faster than one frame per flick, and sample rates, channel
  counts, sequence and stream sizes and `Time::from_rational` denominators are `NonZero`.
  `checked_frame_to_time` reports a frame whose start doesn't fit instead of saturating.
  `Timecode` labels a time at a frame rate, using drop-frame (`;` before the frames) for every
  x/1001 rate whose nominal rate is a multiple of 30 (29.97, 59.94, 119.88), which skips two labels
  per 30 nominal frames at each minute but every tenth. Its frames field counts past 255 and its
  hours reach the ends of `Time`. `Timecode` parses from text (`;` in it marks drop-frame), and
  `to_time` turns one back into the start of its frame, refusing fields out of range, a separator
  that doesn't match the rate, a dropped label and a time `Time` can't hold. `Time::from_samples`
  and `Time::to_samples` convert between time and sample indices at a sample rate the same way, flooring, and every common audio rate from
  8 kHz to 192 kHz has an integral sample duration. `SequenceSettings` carries the project-wide
  `sample_rate`, 48 000 by default. A `Track` keeps its clips sorted by start, and since they never
  overlap, by end too, so `clip_at`, `clips_overlapping` and the overlap check on insert
  binary-search them. `Track::insert` (and
  `check_insert`, its non-mutating preview) refuses a clip that `Clip::check` finds invalid
  (`InvalidClip`: no positive length, a timeline or source start before zero, or an end past
  `Time::MAX`) or that overlaps another. `Project::clip_for` builds the clip that would cover a
  whole asset at a start time on a track, checking the stream kind, the duration and the insert
  without mutating, and `Project::place_clip` inserts it. Every clip carries a `ClipId` unique in
  the project. Asset and clip ids come from the project's `NextIds` counters, which only move
  forward: `clip_for` previews with the next clip id without taking it, `place_clip`, `split_clip`
  and `add_asset` take one, and deleting a clip never frees its id. Moves and trims follow the same
  pair of calls: `moved_clip` and `trimmed_clip` compute the result without mutating, while
  `move_clip` and `trim_clip` apply it. A move is refused where it would overlap or where the target
  track's kind has no matching stream. A trim is clamped instead, between the neighbouring clips,
  the ends of the media and a minimum length of one frame. `split_clip` cuts a clip strictly inside
  it, keeping the id on the head and giving the tail a new one. `pasted_clip` and `paste_clip`
  place a copy of an existing `Clip` (its asset and source range) at a track and start under the
  next clip id, with the same checks as placing an asset. `remove_asset` refuses an asset a clip
  still uses (`AssetInUse`), `prune_assets` removes every unused one and returns them, and a
  removed asset's id is never given out again. `ripple_delete_clip` pulls the later
  clips on the same track back by the deleted clip's length, while `delete_clip` leaves a gap.
  `Timeline::add_track` inserts a track after the last one of its kind. `remove_track` only takes an
  empty track that isn't the last of its kind, and `swap_tracks` only swaps tracks of the same kind.
  Later video tracks sit on top of earlier ones, so `Timeline::video_clips_at` lists the video clips
  under a time from the bottom track up, the order they composite in, and `top_video_clip_at` is its
  last. `History` is the undo stack: `History::apply` runs one edit as a `Command`, keeps a snapshot
  of the project from before it when the edit succeeds and changes something, and rolls the project
  back when the edit fails. `undo` and `redo` swap those snapshots in, carrying the newer `NextIds`
  over so an undone clip or asset never gives its id out again. The stack keeps the last
  `HISTORY_DEPTH` commands, and a new command clears the redo stack. `can_undo`, `can_redo`,
  `next_undo` and `next_redo` describe the next step before it is taken. Every project state the
  history reaches gets a `Revision` never given out again, which undo and redo carry with their
  snapshots; `mark_saved` records the revision a save wrote, so `is_saved` holds exactly when undo
  and redo return to it.
- **`tessera-document`** reads and writes project files (`.tessera`, `EXTENSION`): pretty JSON in
  an envelope `{ "format": "tessera-project", "version": N, "project": … }`, with times as integer
  flicks. The model stays serde-free: each format version has its own DTO module (`v1.rs`, aliased
  as `current`) with conversions to and from `Project`. Loading parses to a `serde_json::Value`,
  checks the marker, runs the `MIGRATIONS` chain (one `fn(Value) -> Result<Value, String>` step per
  source version, so a new version appends a step, and `CURRENT_VERSION` follows its length) and
  then deserializes the current DTO. While Tessera is in early development the format stays at
  version 1 and changes break it in place, fixture included, instead of adding a migration step.
  Rebuilding the `Project` validates rather than trusts: clips go through `Clip::check` and
  `Track::insert`, and ids (unique, and each below the stored next id), assets, stream kinds, clip
  ranges, rates, sizes and channel counts (none zero), asset durations (positive), stream indices
  (unique within an asset) and one track of each kind are checked, each failure a distinct
  `ValidationError`.
  `to_string` and `from_str` are pure. `save` follows symlinks to the file they point at (refusing a
  loop, `SymlinkLoop`), writes a synced sibling temp file of its own (`create_new`, named by pid and
  a process-wide counter) carrying the replaced file's permissions, renames it over the target and
  syncs the directory. `open` and `save` errors carry the path. A fixture in
  `fixtures/v1.tessera` pins the v1 format.
- **`tessera-media`** is the only crate allowed to touch FFmpeg (`ffmpeg-next`, bindgen against the
  system FFmpeg). It covers probing, hwaccel discovery and video and audio decode, and encode goes
  here. Probing leaves out a stream whose size, sample rate or channel count FFmpeg reports as zero,
  which the model can't hold. `VideoDecoder::frame_at` returns a shared packed BGRA `VideoFrame`: it decodes forward from the
  current position unless the stream index shows a keyframe past it, and keeps recent frames in a
  byte-bounded LRU cache keyed by the span each frame covers. FFmpeg types do not cross its public
  API, apart from the `FfmpegError` re-export. A decoder refuses a stream whose time base has a
  part that isn't positive (`InvalidTimeBase`), and converts through the checked `TimeBase`.
  `VideoDecoder` is `Send` so it can move to a background task: ffmpeg-next leaves its scaler
  context `!Send`, but an `SwsContext` has no thread affinity and each one is owned by a single
  decoder, so `Scaler` implements `Send` by hand. A requested time maps to the nearest stream
  tick (`TimeBase::to_ts`), so a 29.97 fps frame start in a 1/1000 time base finds its own frame
  and not the one before. Decoders use frame and slice threads on every core; on a short file that
  reads the whole stream ahead and drains the decoder, which is why the seek-policy test opens one
  with a single thread. `VideoDecoder::open` decodes in hardware through the first of
  `PREFERRED_HW_ACCELS` (VAAPI, then Vulkan Video) that the codec has a device config for and whose
  device can be created; `open_with` takes the list, and an empty one decodes in
  software. Each device is created once per process and shared by every decoder, and a failed
  creation is remembered too. FFmpeg's default `get_format` picks the hardware format, and falls
  back to software by itself when the hardware refuses the stream (a codec profile or size the
  driver lacks), so `hw_accel` reports the accelerator only while decoded frames really come from
  it. Frames stay on the GPU while decoding forward, with `extra_hw_frames` covering the two the
  decoder holds, and only the frame being shown is downloaded (`av_hwframe_transfer_data`) before
  scaling to BGRA. The scaler converts YUV to full-range RGB with the frame's tagged matrix and
  range (`sws_setColorspaceDetails`, read before the download), reading an untagged matrix as BT.709
  from 1280×720 up and BT.601 below, and a `yuvj` format as full range. Each scaler keeps its
  output frame, and rows are copied out of it whole. The test fixture is 128×96 so hardware accepts
  it and can be generated at any frame rate (`generate_at_rate`), and an H.264 variant, generated
  when `libx264` is present, runs the decode tests through the preferred accelerators against
  software.
  `AudioDecoder::samples` returns an `AudioBuffer` of exactly the requested number of interleaved
  stereo `f32` frames at the rate the decoder was opened with, silent before the stream starts and
  past its end. It resamples through swresample, whose context ffmpeg-next already makes `Send`:
  mono is resampled alone and duplicated to both channels, more channels are downmixed. The first
  frame after a seek is placed by its pts, and the output then continues sample by sample, so a
  read starting where the last one ended never seeks. A read behind the last one, or more than a
  second past what is decoded, seeks 100 ms early (flushing the decoder and the resampler) and
  discards up to the target.
- **`tessera-audio`** plays the timeline's sound through PipeWire (`pipewire`, the only crate
  allowed to touch it). `Mixer::render` fills a block of interleaved stereo `f32` samples starting
  at a timeline sample index: every clip on every audio track that overlaps the block (`clips_overlapping`) is read
  from an `AudioDecoder` (one per media path, at the project's sample rate) and summed into its
  part of the block. A clip reads from its source start plus its offset into the clip, so blocks
  that follow each other continue in the source without seeking. Media that fails to open or
  decode is warned about once and stays silent. `Output` runs a PipeWire playback stream on its
  own thread, whose process callback copies whole frames out of a shared queue (silence on
  underrun), and a feeder thread that keeps about 200 ms queued by calling the source closure.
  Its `position` is the audio clock: the samples the device has played, taken at each callback
  as the samples consumed minus the stream's delay to the device (`pw_time` delay plus
  resampler buffering), interpolated between callbacks with a monotonic clock, capped at what was
  consumed and never running backwards. Silence from an underrun doesn't advance it.
  `TimelinePlayback` ties a `Mixer` to an `Output` from a start time, reports the time played
  since then, and picks up a replaced project on the next block. Dropping the `Output` stops the
  stream thread and joins it, and the feeder exits after its current block.
- **`tessera-render`** owns a `wgpu` Vulkan `Compositor` for compositing timeline frames. It
  doesn't depend on `tessera-media`: a `Layer` borrows a packed straight-alpha BGRA8 image shaped
  like `VideoFrame`, and `Compositor::composite` returns an owned sequence-sized `Frame`. It clears a
  `Bgra8Unorm` target to opaque black, draws the layers bottom first as one textured quad each,
  blended over with their alpha, and reads the target back with its rows unpadded. Each layer is
  scaled uniformly to fit inside the sequence and centred (`fit::fit_rect`, letterboxing or
  pillarboxing), with linear filtering, and a layer of the sequence's size comes back byte for
  byte. The target and readback buffer are kept while the sequence size holds, and layer
  textures, each with its own placement uniform and bind group, are pooled by size, keeping only
  those the last call used. The `Viewer` is its only user.
- **`tessera-ui`** holds the GPUI views. `Workspace` owns an `Entity<Project>` through its `ProjectEditor`, and each panel
  (`MediaBin`, `Viewer`, `TimelinePanel`) gets a clone of it and re-renders through
  `cx.observe(&project, …)`. Every edit goes through the `ProjectEditor` (`editor.rs`), which
  pairs the project entity with its `Entity<History>`: `apply` and `perform` record the edit as a
  command and notify the project, so every panel updates and Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y can
  undo and redo it. The `MediaBin` and `TimelinePanel` get a clone of it, and imports are commands
  too. Never mutate the project entity directly. Ctrl+S saves to the file the project was last
  saved to or opened from, or asks for one (appending `.tessera`), and Ctrl+O opens one; the
  `Workspace` does the file IO on the background executor through `save_to` and `open_from`, one
  job at a time in the order they were asked for (`queue_file_io`), reports failures in a prompt,
  and titles the window after the file. A save marks the revision it snapshotted, not the one
  current when it finishes, and the `Workspace` observes the history to mark the title (`• `)
  while the project differs from that revision. While it does, Ctrl+Q, closing the window and
  opening another project ask first (`confirm_discard`): Save saves it (through the dialog when it
  has no file) and goes on only once the save succeeds, Don't Save goes on, Cancel stops. Ctrl+O
  asks only after the chosen file has loaded, and a second request while one prompt is up is
  refused. Opening swaps the project
  into the existing entity through `ProjectEditor::replace`, which clears the history, and pauses
  the playhead at zero. Through their `project_replaced`, the timeline drops its selection and
  view, and the bin its pending imports and thumbnails before decoding the new assets' ones. Global
  actions and keybindings are registered in `tessera_ui::init`; actions that need the project are
  handled on the focused `Workspace`, whose key context (`WORKSPACE_CONTEXT`) scopes the
  single-key transport bindings. The `Workspace` also owns an `Entity<Playhead>` shared the same
  way: the playhead is UI state, not part of the project, and scrubbing snaps it to frame starts.
  The playhead also runs the transport: space plays and pauses, J/K/L shuttle (each press doubles
  the speed up to 8× in that direction) and the arrow keys step one frame. Playing anchors a
  clock at the start time and a ticker task moves the playhead to the frame start under
  `anchor + elapsed × speed`, so it never drifts, and stops on the timeline's last frame or at
  zero. At normal forward speed the clock is a `TimelinePlayback`, so the audio clock drives the
  picture, and the playhead passes each project change on to it. Other speeds, or a failed
  audio output (warned about), use the wall clock and play no sound. Any seek or step pauses playback. The timeline's ruler is a `canvas` that paints its ticks
  and timecode labels and scrubs the playhead through window mouse listeners, so a drag keeps
  tracking outside the ruler. The timeline maps time to pixels through a `Viewport` (zoom as
  pixels per second, plus the time at the left edge). The wheel scrolls it, Ctrl+wheel zooms
  around the pointer, and `=`, `-` and Shift+Z zoom in or out around the playhead or fit the
  timeline. The ruler steps down to single frames at deep zoom. Outside a scrub, the view pages
  to the playhead whenever it leaves the visible range.
  `MediaBin` owns importing (dialog and drops) and probes each file and decodes its thumbnail on a
  background task, reporting failures in the bin itself. Thumbnails are UI state kept in the bin by
  `AssetId`, not part of the project model. Bin rows drag a `DraggedAsset`: each timeline lane
  previews the drop as a ghost (red where it would overlap) through `on_drag_move`, and the drop
  places the clip at the previewed, frame-snapped start. Clips on the timeline work the same way.
  Pressing a lane selects the clip under the pointer, or clears the selection, and records where
  the clip was grabbed. Dragging a clip's body or one of its edge handles starts a `DraggedClip`
  drag that the lanes preview and commit as a move or a trim. While snapping is on (N toggles it),
  a dropped asset's edges, a moved clip's edges and a trimmed edge pull onto the nearest clip edge,
  the playhead or zero within a few pixels (`timeline::snap`), and the lanes draw a line where the
  preview snapped. Ctrl+K splits the selected clip at the
  playhead, or every clip under it when the selection is elsewhere or empty. Delete or Backspace
  deletes the selection, and with Shift held they ripple delete it. The timeline draws video tracks
  from the highest index down, then audio tracks in order (`header::display_order`). Each track
  header has buttons to swap it with its same-kind neighbour or remove it, and a row below the
  tracks adds video or audio tracks. Shift+wheel scrolls the track rows vertically, under a fixed
  ruler. Panel interactions are tested
  headlessly with GPUI's `test-support` (`#[gpui::test]` and `VisualTestContext` mouse
  simulation). The `Viewer` requests every video clip under the
  playhead as a layer, bottom to top. A background job decodes each at sequence size and
  composites them into a sequence-sized frame, letterbox bars included, which becomes the GPUI
  image. The job carries one decoder per media path and size, dropping decoders and frames of
  media the project no longer holds, and a `Compositor` it creates on first use off the UI thread.
  If that creation fails it warns once and shows the top layer's decoded frame alone from then on.
  One job runs at a time, so while scrubbing only the newest request runs next, and the viewer
  releases each replaced frame from the GPUI atlas with `Window::drop_image`. While playing, it
  asks for the frame at the time it will reach the screen instead of the playhead's: the playhead
  plus the render latency (a smoothed average of recent jobs) times the speed, clamped to the
  timeline (`presentation_time`). A frame that lands early is held until the playhead reaches it,
  one overtaken by a newer frame is dropped, and a change of speed drops a held frame
  (`fate`), so a slow decode or composite shows fewer frames rather than lagging the audio.
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

Building needs the system FFmpeg and PipeWire development headers (the build runs bindgen, so it
needs clang) and a Vulkan driver.

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
