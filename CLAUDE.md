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
  `tessera-media` has to call into FFmpeg to describe a clip. A `VideoStream` records its coded
  size, rate, display `Rotation` (a quarter turn, clockwise), `PixelAspect` (lowest terms, `SQUARE`
  by default), pixel format, `default` disposition and `start` (its offset from the container's
  start); `display_size` stretches the width by the pixel aspect and then turns it. An
  `AudioStream` records its `default` disposition and `start` too, and `default_video` and
  `default_audio` pick the stream marked default, else the first. A clip's `audio_stream` chooses
  which of its asset's audio streams it plays (`None` plays the default, `Asset::audio_stream_for`),
  set through `set_clip_audio_streams` or stepped with `cycle_audio_streams`, refusing a stream the
  asset lacks (`UnknownAudioStream`), as `relink_asset` does for media without a chosen stream. It has no IO and no GPU, and it
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
  `sample_rate`, 48 000 by default, and `Project::set_settings` swaps the whole set in and returns the old
  one, as the `SetSequenceSettings` command. A `Track` keeps its clips sorted by start, and since they never
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
  next clip id, with the same checks as placing an asset. `relink_asset` swaps an asset's path and probed info when every clip still fits (`BeyondMedia`,
  `MissingStream`). `remove_asset` refuses an asset a clip
  still uses (`AssetInUse`), `prune_assets` removes every unused one and returns them, and a
  removed asset's id is never given out again. `delete_clips`, `ripple_delete_clips`
  and `move_clips` edit several clips by id in one call (a group move may land on the clips it
  leaves, and a refused one changes nothing). `insert_asset` and `insert_clip` split the clip
  under the start and push the later clips on the track right, `overwrite_asset` and
  `overwrite_clip` trim or remove what the new clip covers, `ripple_trim_clip` trims an edge and
  moves the later clips with it, `roll_clips` moves the cut between two touching clips, `slip_clip`
  moves a clip's source window and `slide_clip` moves a clip while its touching neighbours give or
  take the time. These live in `project/edits.rs`, and every one that spans several clips restores
  the timeline and the id counters when it fails. A clip may carry a `LinkId` (from `NextIds`,
  never reused) shared with its partners, the clips of one asset that play together; linked clips
  always share their asset, start and source range (`project/links.rs`). `place_linked` (previewed
  by `linked_clips_for`) places an asset's clip and, when the asset has the other kind of stream
  and the `partner_track` (the other kind's track of the same ordinal, else its first) accepts it,
  a linked partner there, refusing the whole placement when either overlaps. `move_clips` (and so
  `move_clip`) moves partners by the same time on their own tracks, `trim_clip` trims them to the
  same edge as far as every one of them can go, `split_clips` (and `split_clip`) splits them all
  and links the tails under a new id, and `delete_clips` and `ripple_delete_clips` (and their
  single-clip forms) take the partners too, so a ripple pulls every partner's track.
  `paste_clips` links the copies of partners pasted together under a new id. `insert_linked` and
  `overwrite_linked` insert or overwrite an asset's clip and its partner on the
  `linked_partner_track` the same way, linked, and an overwrite that cuts a linked clip in two
  links the tails on every track under one new id. `unlink_clips` unlinks the groups of the given
  clips, and `link_clips` links again every set of the given clips that share an asset, start and
  source range (`NothingToLink` when none do), dropping a link that is left on one clip; any other edit that leaves partners out of step (an
  insert or overwrite that shifts or cuts one, a ripple trim, roll, slip or slide) drops their
  link (`settle_links`). Every clip has a `gain` and every track a `volume`, both a `Gain` in
  tenths of a decibel from `Gain::SILENT` (−60 dB, played as silence) to `Gain::LOUDEST` (+12 dB),
  `UNITY` by default; `adjust_clip_gains` moves the gain of several clips at once, saturating at
  those ends. A `Track` carries a `name`
  (empty shows the default label), `locked`, `muted`, `solo`, its `volume` and a `TrackHeight`; a locked track
  refuses every edit of its clips and every placement onto it (`TrackLocked`), through
  `located_clip` and `track_accepting`. A muted video track is left out of the composite
  (`video_clips_at`), a muted audio track is silent, and `solo` applies to audio tracks only. The project holds `markers` (sorted by time, with
  `MarkerId`s from `NextIds` that are never reused; `add_marker`, `move_marker`, `rename_marker`,
  `remove_marker`, `next_marker_after`, `previous_marker_before`) and an `in_point` and
  `out_point` that stay in order (`set_in_point`, `set_out_point`, `clear_in_out`). `ripple_delete_clip` pulls the later
  clips on the same track back by the deleted clip's length, while `delete_clip` leaves a gap.
  `Timeline::add_track` inserts a track after the last one of its kind. `remove_track` only takes an
  empty track that isn't the last of its kind, and `swap_tracks` only swaps tracks of the same kind.
  Later video tracks sit on top of earlier ones, so `Timeline::video_clips_at` lists the video clips
  under a time from the bottom track up, the order they composite in, and `top_video_clip_at` is its
  last. `Project::assets` and each `Track`'s clips are
  `Arc`s that are copied on write, so the snapshots `History` keeps share whatever an edit leaves
  alone, and comparing a snapshot with the project is a pointer check for those parts. An asset
  with a video stream and no duration is a still (`Asset::is_still`): `clip_for` gives it
  `Asset::STILL_DURATION`, and its clips can be trimmed out to any length the neighbours allow. An
  asset with no duration and no video still refuses with `NoDuration`. `History` is the undo stack: `History::apply` runs one edit as a `Command`, keeps a snapshot
  of the project from before it when the edit succeeds and changes something, and rolls the project
  back when the edit fails. `undo` and `redo` swap those snapshots in, carrying the newer `NextIds`
  over so an undone clip or asset never gives its id out again. The stack keeps the last
  `HISTORY_DEPTH` commands, and a new command clears the redo stack. `can_undo`, `can_redo`,
  `next_undo` and `next_redo` describe the next step before it is taken. Every project state the
  history reaches gets a `Revision` never given out again, which undo and redo carry with their
  snapshots; `mark_saved` records the revision a save wrote, so `is_saved` holds exactly when undo
  and redo return to it, and `mark_unsaved` (a recovered autosave) leaves no revision saved.
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
  (unique within an asset), rotations (quarter turns), pixel aspects (no zero part), each clip's
  chosen audio stream (one its asset has), one track of each kind and the links (each issued, held by at least two
  clips on different tracks, in step) are checked, each failure a distinct `ValidationError`.
  `to_string` and `from_str` are pure and leave media paths as they are. `save` and `open` make them
  portable: a media path under the project file's directory (the directory of the file a symlink
  points at) is stored relative to it, any other path is stored absolute (a path relative to the
  working directory included), and `open` joins a relative stored path onto that directory. `save` follows symlinks to the file they point at (refusing a
  loop, `SymlinkLoop`), writes a synced sibling temp file of its own (`create_new`, named by pid and
  a process-wide counter) carrying the replaced file's permissions, renames it over the target and
  syncs the directory. `open` and `save` errors carry the path. A fixture in
  `fixtures/v1.tessera` pins the v1 format.
- **`tessera-media`** is the only crate allowed to touch FFmpeg (`ffmpeg-next`, bindgen against the
  system FFmpeg). It covers probing, hwaccel discovery and video and audio decode, and encode goes
  here. Probing leaves out a stream whose size, sample rate or channel count FFmpeg reports as zero,
  which the model can't hold, an attached picture (cover art) and a stream whose decoder cannot be
  set up (warned about), instead of failing the import. It reads the rotation from the stream's
  display matrix (`av_display_rotation_get` counts counterclockwise), the pixel aspect through
  `av_guess_sample_aspect_ratio` and the start offset against the container's start. `VideoDecoder::frame_at` returns a shared packed BGRA `VideoFrame`: it decodes forward from the
  current position unless the stream index shows a keyframe past it, and keeps recent frames in a
  byte-bounded LRU cache keyed by the span each frame covers; `is_cached` says whether a time would be answered
  from it, without changing the eviction order. FFmpeg types do not cross its public
  API, apart from the `FfmpegError` re-export. A decoder refuses a stream whose time base has a
  part that isn't positive (`InvalidTimeBase`), and converts through the checked `TimeBase`.
  `VideoDecoder` is `Send` so it can move to a background task: ffmpeg-next leaves its scaler
  context `!Send`, but an `SwsContext` has no thread affinity and each one is owned by a single
  decoder, so `Scaler` implements `Send` by hand. A requested time maps to the nearest stream
  tick (`TimeBase::to_ts`), so a 29.97 fps frame start in a 1/1000 time base finds its own frame
  and not the one before. Every stream is offset by the container's start time
  (`stream_start`, falling back to the stream's own), so sound and picture stay aligned when the
  streams start apart. A request before the stream's first frame is answered from that frame
  without seeking again, and a forward gap seeks when the stream index ends short of the target
  (`forward_seek_needed`) instead of decoding through it. Packets are read with `read_packet`, so
  a terminal IO error is reported as a decode error and not as the end of the file, and a
  `receive_frame` error other than EAGAIN or end of stream is a decode error too. A hardware
  decoder that fails to open, or fails mid-stream, is replaced by a software one (`hw_accel`
  turns `None`), and a hardware device that failed to be created is tried again after 30 s. Decoders use frame and slice threads on every core; on a short file that
  reads the whole stream ahead and drains the decoder, which is why the seek-policy test opens one
  with a single thread. `VideoDecoder::open` and `AudioDecoder::open` take the index of the stream
  to decode, the one probing recorded in `MediaInfo`, and refuse an index that isn't a stream of
  their kind (`NoVideo`, `NoAudio`); the media bin's thumbnails decode the default video
  stream. `VideoDecoder::open` decodes in hardware through the first of
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
  output frame, and rows are copied out of it whole. A frame comes out in its display shape: the
  scaler stretches the width by the stream's pixel aspect (`Shape`, read at open as probing reads
  it), fitting bounds swapped when the rotation turns the picture on its side, and the packed rows
  are then turned upright (`turned`), so `VideoFrame`'s size is the display size. The test fixture is 128×96 so hardware accepts
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
  at a timeline sample index: every clip on every audible audio track (not muted, and when any audio track is soloed only the
  soloed ones) that overlaps the block (`clips_overlapping`) is read
  from an `AudioDecoder` (decoding the clip's audio stream, `audio_stream_for`, at the project's sample
  rate), scaled by the clip's gain times its track's volume (a silent one is skipped) and summed
  into its part of the block; the sum then goes through a soft limiter that leaves samples up to ±0.9 alone
  and eases the rest toward ±1.0. A clip's first sample is the first one at or after its start
  (`ceil_samples`), and the source sample for every later one is counted from there in whole
  samples (`AudioDecoder::samples_from`), so blocks that follow each other continue in the source
  without seeking or repeating a sample even when the clip starts between samples. Decoders are
  kept per media path, stream and track, so two clips of one file overlapping in time do not seek
  each other. Media that fails to open or decode is warned about once per path and stream and
  stays silent. `Output` runs a PipeWire playback stream on its
  own thread, whose process callback copies whole frames out of a shared queue (silence on
  underrun), and a feeder thread that keeps about 200 ms queued by calling the source closure
  with the index of the block's first frame (the frames consumed plus those queued). `flush` cuts
  the queue to its first 20 ms and bumps a generation, so a block rendered across it is dropped
  and the next one starts right after what was kept.
  Its `position` is the audio clock: the samples the device has played, taken at each callback
  as the samples consumed minus the stream's delay to the device (`pw_time` delay plus
  resampler buffering), interpolated between callbacks with a monotonic clock, capped at what was
  consumed and never running backwards. Silence from an underrun doesn't advance it.
  `TimelinePlayback` ties a `Mixer` to an `Output` from a start time, reports the time played
  since then, and picks up a replaced project on the next block, flushing the queue when the
  replacement changes what is heard (timeline, assets or settings; markers and in and out points
  don't), so an edit is heard within about 20 ms. Dropping the `Output` stops the
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
  saved to or opened from, or asks for one (appending `.tessera`, and asking before it replaces an
  existing file the dialog did not know about), Ctrl+Shift+S always asks, Ctrl+O opens one and
  Ctrl+N starts an empty `NEW_PROJECT_NAME` project (after the same unsaved-changes prompt); the
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
  `anchor + elapsed × speed`, so it never drifts. Forward it plays on until the timeline's end, so the
  last frame is heard too, and then stops on that frame; backward it stops at zero. At normal forward speed the clock is a `TimelinePlayback`, so the audio clock drives the
  picture, and the playhead passes each project change on to it. Other speeds, or a failed
  audio output (warned about), use the wall clock and play no sound. An audio clock that stops
  advancing for a second (never started, or the stream died) is warned about and replaced by the
  wall clock, continuing from where it stopped (`WatchedAudio`), and a change of the sequence's
  sample rate while the audio clock runs starts a new output from the playhead. Any seek or step pauses playback. The timeline's ruler is a `canvas` that paints its ticks
  and timecode labels and scrubs the playhead through window mouse listeners, so a drag keeps
  tracking outside the ruler. The timeline maps time to pixels through a `Viewport` (zoom as
  pixels per second, plus the time at the left edge). The wheel scrolls it, Ctrl+wheel zooms
  around the pointer, and `=`, `-` and Shift+Z zoom in or out around the playhead or fit the
  timeline. The ruler steps down to single frames at deep zoom. Outside a scrub, the view pages
  to the playhead whenever it leaves the visible range.
  `MediaBin` owns importing (dialog and drops). Probes, folder scans and thumbnail decodes are jobs
  on one queue, at most `MAX_RUNNING_JOBS` running on background tasks at once; replacing the project
  clears the queue and bumps a generation so that jobs still running are ignored. A probed asset
  joins the project at once, and the bin then queues its thumbnail (decoded in software at
  thumbnail size) for every video asset that has none, so a thumbnail also returns after an undo
  brings the asset back and is released from the atlas when the asset goes. A dropped folder is
  scanned for files with a media extension (`MEDIA_EXTENSIONS`), leaving out images such as cover
  art. A file chosen in the dialog (which can't filter by type) or dropped on its own is refused
  without probing unless it has a media or still-image extension (`STILL_EXTENSIONS`). An import
  is skipped when its path, or its canonical path, matches an asset's path or the canonical path
  the presence check resolved for it; the probe and the presence check canonicalize on their
  background jobs, and a finished import records its canonical path at once, so two paths to one
  file dropped together import it once. A path that is not valid UTF-8 is refused, since a
  project could not save it. Failures show as one dismissible row. Each row shows the asset's metadata (duration, size, codec
  and rate), is marked Unused when no clip uses it and Missing when a background check found the
  file gone, and has a Remove button (refused while a clip uses the asset) or, when missing, a
  Relink button that probes a chosen file and swaps it in through `relink_asset`, which refuses
  media that lacks a stream a clip needs or ends before a clip does. A Remove unused button
  appears while there are unused assets. Thumbnail and presence checks are keyed by asset id and
  path, so a relinked asset gets both again. A search field (Ctrl+F) filters the assets by name and a button cycles their order
  between added, name and length (`visible_assets`). Thumbnails are UI state kept in the bin by
  `AssetId`, not part of the project model. Bin rows drag a `DraggedAsset`: each timeline lane
  previews the drop as a ghost (red where it would overlap) through `on_drag_move`, and the drop
  places the clip at the previewed, frame-snapped start. Clips on the timeline work the same way.
  The selection is a set of clips. Pressing a lane selects the clip under the pointer (keeping the
  selection when that clip is already in it, so a group can be dragged) or clears the selection,
  and with Shift held toggles the clip in the selection instead. Pressing empty lane space starts a
  marquee (`Marquee`, kept in time and content-y so it holds while the view scrolls) that selects
  every clip it touches on the rows it spans, added to the selection when Shift was held. Ctrl+A selects every clip. It
  records where the clip was grabbed. The panel drops clips that no longer exist from the
  selection whenever the project changes, and pulls the view back when the timeline shrinks under
  it. Pressing, Shift-toggling and sweeping select a linked clip together with its partners (Ctrl+L
  unlinks the selection when it holds a linked clip and links it otherwise). Dragging a selected clip's body moves the whole selection by the same
  time (`move_clips`), carrying the clips on the dragged clip's track to the lane it is dropped on
  and leaving the others on their tracks, previewed as a ghost for every selected clip
  (`DropPreview::group`, `ghosts_on`), all red when any selected clip would collide. A plain drop
  of an asset places it linked with its sound or picture (`place_linked`), and the preview draws
  the partner's ghost on its lane (`DropPreview::partner`), as a trim of a linked clip does. A trim keeps the offset at which its
  handle was grabbed and commits wherever the pointer is released, since only the clip's own lane
  previews it. Dropping an asset with Ctrl held inserts it and with Alt held overwrites
  (`DropMode`, carried on the `DropPreview`), linked with its partner like a plain drop; a red ghost says why it was refused. A start that
  snapped by its end is floored to the frame grid. Each lane lays out only the clips overlapping
  the visible time range, and trim handles are at most a third of the clip's width. Dragging a clip's body or one of its edge handles starts a `DraggedClip`
  drag that the lanes preview and commit as a move or a trim. While snapping is on (N toggles it),
  a dropped asset's edges, a moved clip's edges and a trimmed edge pull onto the nearest clip edge,
  the playhead or zero within a few pixels (`timeline::snap`), and the lanes draw a line where the
  preview snapped. Ctrl+K splits the selected clips at the
  playhead, or every clip under it when none of the selection is there. Delete or Backspace
  deletes the selection, and with Shift held they ripple delete it. The timeline draws video tracks
  from the highest index down, then audio tracks in order (`header::display_order`). Each track
  header shows the track's name (or its numbered label), has buttons to swap it with its same-kind
  neighbour or remove it (the disabled remove button's tooltip says why), toggles for lock, mute
  (hide on a video track) and solo (audio only) and a button that cycles the row height between
  compact, normal and tall (`header::row_height`), and a row below the tracks adds video or audio
  tracks. An audio track's header shows its volume beside the toggles (not on a compact row):
  scrolling it steps a decibel up or down and a double-click resets it. Alt+Up and Alt+Down raise
  and lower the gain of the selected audio clips by a decibel, and a clip whose gain isn't unity
  shows it after its name. Alt+S steps the selected audio clips to their asset's next audio stream,
  and an audio clip of media with several shows which one it plays (`A2`). Split and delete skip the clips of locked tracks. Double-clicking a track's name edits it in a `TextField`. Ctrl+C, Ctrl+X, Ctrl+V and Ctrl+D copy, cut, paste
  and duplicate the selection: the panel keeps a clipboard of clips with their tracks, a paste
  lands at the playhead (a duplicate right after the selection) keeping the clips' relative
  offsets and tracks, as one `paste_clips` command that is refused whole when anything overlaps or
  a track is locked. M adds a marker at the playhead and Shift+M removes the one there, I and O set
  the in and out points and Alt+X clears them; markers draw as flags on the ruler, with
  their names beside them, and the in and out points as a shaded range. Pressing the ruler on a
  flag (its bottom band, within the flag's width) grabs the marker instead of scrubbing: a drag
  previews it on frame starts and the release commits one `MoveMarker`, and a double-click names
  it in a `TextField` (`RenameTarget::Marker`, sharing the track rename's field handling). Down and Up jump the playhead to the next and previous edit
  (`Timeline::next_edit_after`, the starts and ends of all clips), Ctrl+Right and Ctrl+Left to the
  next and previous marker, and Escape cancels a drag in progress. Shift+wheel scrolls the track rows vertically, under a fixed
  ruler. A horizontal scrollbar under the lanes and a vertical one at their right
  (`timeline/scrollbar.rs`, a `Thumb` as fractions of the extent, hidden when everything fits) are
  dragged by the grabbed point, and a press beside the thumb centres it there. A drag over the
  lanes keeps its pointer as a `DragHover` (lane, offset, what is dragged), so whenever the view
  scrolls mid-drag, by the wheel or by autoscroll, the preview is recomputed and the grabbed point
  stays under the pointer; within `AUTOSCROLL_EDGE` of either side of the lanes a ticker scrolls
  the view, faster the closer to the edge, up to a visible width past the timeline's end. Panel interactions are tested
  headlessly with GPUI's `test-support` (`#[gpui::test]` and `VisualTestContext` mouse
  simulation). The `Viewer` requests every video clip under the
  playhead as a layer, bottom to top (`Timeline::video_layers_at`, which leaves out muted tracks).
  A background job decodes the layers in parallel, one thread each, at the render size and
  composites them into a frame of that size, letterbox bars included, which becomes the GPUI image.
  The render size is the sequence scaled down uniformly (in steps of an eighth) to the viewer's
  frame, never above the sequence (`render_bounds`), and a change of it reopens the decoders. The
  job carries one decoder per media path, stream (the asset's default video stream), track and
  size, closing those whose track no longer
  holds a clip of that media, so two clips of one file on different tracks never share one and
  split clips on one track do. Media that fails to open is remembered per decoder key and warned
  about once, and the layers that did decode still show. A single layer that already has the
  render size skips the compositor. The job creates a `Compositor` on first use off the UI thread;
  if that fails it warns once and shows the top layer's decoded frame alone from then on, and
  after a compositing error it shows that frame, recreates the compositor and gives up after three
  errors in a row. A panic in a job is caught: the viewer shows a failure and starts a new
  renderer. The apostrophe key toggles safe-area outlines at 90 % and 80 % of the frame. A gap in the timeline shows black (`Picture::Black`), and a clip that is deleted or
  moved off its track blacks the picture at once. A finished render is shown only while the
  playhead is still at it (when paused it must also still match the wanted frame). One job runs at a time, so while scrubbing only the newest request runs next, and the viewer
  releases each replaced frame from the GPUI atlas with `Window::drop_image` as soon as it is
  replaced, deferred out of the current update through the window handle the viewer keeps. While playing, it
  asks for the frame at the time it will reach the screen instead of the playhead's: the playhead
  plus the render latency (a smoothed average of recent jobs that decoded something: a job whose
  layers all came from the decoders' caches or from remembered failures is left out) times the
  speed, clamped to the
  timeline (`presentation_time`). A frame that lands early is held until the playhead reaches it,
  one overtaken by a newer frame is dropped, and a change of speed drops a held frame
  (`fate`), so a slow decode or composite shows fewer frames rather than lagging the audio.
  `TextField` (`text_field.rs`) is a single-line text input built on key events: it appends the
  typed character, backspace deletes, Enter submits and Escape cancels, emitting
  `TextFieldEvent`s, and it ignores keys with Ctrl or Alt. Every workspace binding is scoped to
  `Workspace && !TextField && !Dialog` (`SHORTCUT_CONTEXT`), so typing in a field or moving
  through a dialog never triggers a shortcut; a dialog without fields keys its own context
  (`DIALOG_CONTEXT`), where Up, Down, Enter and Escape are `SelectPrevious`, `SelectNext`,
  `Confirm` and `Dismiss`. The `Workspace` shows one dialog at a time as an overlay (`Dialog`).
  Ctrl+, opens the sequence settings dialog (`SequenceSettingsDialog`): width and height fields
  (Enter moves from one to the next and applies), the standard frame rates and a few sample rates
  as choices, applied through `SetSequenceSettings`. Ctrl+Shift+O opens the recent projects
  (`RecentProjectsDialog`), and choosing one opens it like Ctrl+O. `RecentProjects` (`recent.rs`)
  is a GPUI global: every file a project is saved to or opened from moves to the front of it (at
  most `MAX_RECENT_PROJECTS`, UTF-8 paths only), and a file that fails to open drops out. It is
  in memory unless `use_state_directory` loaded it from `$XDG_STATE_HOME/tessera/recent-projects`
  (`~/.local/state` without one, one path per line), which it is then written back to on a
  background task after each change. The same call sets the `AutosaveDirectory` global
  (`…/tessera/autosave`); only the binary makes it, so tests never touch the real state directory.
  With that directory set, each `Workspace` claims an autosave `Slot` (`autosave.rs`, named by pid
  and a process-wide counter, `<slot>.tessera` plus a `<slot>.source` holding the project's file)
  and every `AUTOSAVE_INTERVAL` writes the project into it through the file IO queue while it has
  unsaved changes not yet written. A save that leaves the project saved, and Don't Save in
  `confirm_discard`, remove the slot. `open_main_window` calls `offer_recovery`: the newest slot
  whose pid has no `/proc` entry (an orphan of a session that crashed) is offered as Recover,
  Discard or Not Now; Recover asks about the current project's changes, swaps the recovered
  project in, takes the orphan over as the session's own slot and leaves the project marked
  unsaved (`History::mark_unsaved`) under the file it came from.
- **`tessera`** sets up tracing (`RUST_LOG`, `info` by default), initialises the media backend,
  points the UI at the state directory and opens the main window.

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
