## Model

- Link the video and audio clips placed from one asset, so one drop places both and move, trim,
  split, delete and ripple delete keep them in sync (`ripple_delete_clip` shifts only its own track)
- Edit several clips in one command (move, delete, ripple delete) for multi-selection
- Insert, overwrite, roll, slip, slide and ripple trim edits
- Place an existing `Clip` at a track and start, for copy, paste and duplicate
- Remove assets from the project, refused while a clip uses them, and prune unused ones
- Markers and in/out points stored in the project
- Track names, lock, mute, solo and height stored on `Track`
- Give still images and other assets without a duration a default clip length, instead of
  `clip_for` refusing them with `NoDuration`
- Carry rotation, sample aspect ratio, pixel format, stream disposition and per-stream start time in
  `MediaInfo`, and let a clip choose which of several audio streams it plays
- A `SetSequenceSettings` command for resolution, frame rate and sample rate
- Expose `can_undo`, `can_redo`, the next command's name and a saved position on `History`, which
  the dirty indicator and the unsaved-changes prompt both need
- Share unchanged assets and tracks between undo snapshots instead of deep-cloning the whole project
  up to `HISTORY_DEPTH` times, and skip the clone and compare for edits that fail
- `Timecode` edge cases: `as u8` truncation above 255 fps, `abs` of `i64::MIN`, the untested
  120000/1001 drop-frame rate, and parsing a typed timecode back into a time
- Binary-search the sorted clips in `check_free_of_others` and the clip lookups

## Project files

- Ask before discarding unsaved changes on Ctrl+Q (a bare `cx.quit()`), on closing the window and
  on Ctrl+O, which replaces the project and clears the history without asking
- Give each save its own temp file (`create_new`) and serialise saves and opens: two quick Ctrl+S
  share `.{name}.{pid}.tmp`, so one truncates the other or fails its rename, and an older snapshot
  or a slow open can land after newer edits
- Fsync the parent directory after the rename, and save through a symlink to its target while
  keeping the file's permissions
- Validate sequence and stream width, height, sample rate and channels, negative asset durations and
  duplicate stream indices on load (a `"width": 0` sequence loads today)
- Store media paths relative to the project file when the media sits under it, and resolve relative
  paths against the project's directory rather than the working directory
- Refuse non-UTF-8 media paths at import or store them losslessly; such a project can never be saved
- Confirm the overwrite when `.tessera` is appended to a name the save dialog approved without it
- Save As, recent projects and a dirty indicator in the title bar
- Relink media that has moved or gone missing, and mark missing media in the bin
- Autosave and recovery after a crash

## Media decode

- Offset every stream by one container-wide start time instead of each stream's own `start_time`,
  which puts sound and picture out of sync on TS files and MP4s with edit lists
- Round a requested time to the nearest stream tick instead of flooring it: 29.97 fps in a 1/1000
  time base (MKV, WebM) returns the previous frame, so stepping repeats frames
- Set the decoder's thread count and type; software decode runs on a single thread
- Apply the display-matrix rotation and the sample aspect ratio, so phone footage isn't sideways and
  anamorphic DV and HDV aren't squashed
- Seek when the stream index ends before the target, instead of decoding forward through the whole
  gap in MPEG-TS and MKV without cues
- Tell decode errors apart from `EAGAIN` in `receive_frame`, reopen in software when a hardware
  decoder fails to open or mid-stream, and retry a device that failed to create
- Re-anchor audio on packet timestamps after gaps and jumps, not only after a seek, and stop reading
  a terminal IO error from the packet iterator as a clean end of file
- Skip attached-picture streams (cover art) and undecodable streams when probing instead of listing
  them as video or failing the import, and decode the probed stream index rather than `best()`
- Scale into a reused BGRA buffer and copy rows whole, instead of a new frame and a byte-by-byte
  `flat_map` copy per converted frame
- Decode thumbnails in software at thumbnail size rather than through a hardware decoder at full size
- Stop seeking again on every request for a time before the stream's first frame
- Keep a decoded run of frames for backward stepping and reverse shuttle, and budget frame caches
  across decoders instead of 256 MiB each
- Deinterlace interlaced sources, and interpolate chroma fully in the scaler

## Playback and audio

- Fall back to the wall clock and warn when the audio stream never runs: without a sink, after a
  PipeWire restart or after a panic in the feeder the audio clock stands still and so does the
  playhead
- Keep one PipeWire connection for the session instead of connecting on every play, blocking the UI
  thread on it, and joining the stream thread on every pause
- Play the sound of video clips, through linked audio clips or their own stream
- Mix the audio tracks, with per-clip gain and per-track volume, mute and solo
- Limit the summed mix, which exceeds ±1.0 wherever clips overlap
- Flush the queued audio when the project changes while playing, instead of playing up to 200 ms of
  the old timeline
- Give each clip its own read position, so two clips of one file overlapping in time don't seek on
  every block
- Carry the fraction of a clip start that isn't sample-aligned (NTSC frame starts), which repeats
  one sample at the clip's first block boundary
- Restart the output when the sequence's sample rate changes while playing
- Play sound at shuttle speeds, in reverse and while scrubbing or stepping
- Play the last frame's sound at the end of the timeline instead of stopping at its start
- Retry media that failed during playback, and open decoders ahead of clip starts with reused mix
  buffers instead of on the feeder thread at the first block
- Draw waveforms on audio clips

## Viewer

- Render at the viewer's size instead of the sequence's: a 4K sequence decodes, uploads, reads back
  and atlases about 33 MB a frame for a panel a few hundred pixels wide
- Recover when a render job panics or the device is lost: the renderer moved into the job never
  comes back and the picture freezes for the session, and `composite` errors have no fallback
- Show black in gaps and past the last clip, instead of flashing the sequence placeholder text
- Discard a finished render that no longer matches the wanted frame: a deleted clip or a seek shows
  the old frame for one job, and a gap keeps the last frame until the job ends
- Show the layers that decoded when one fails, and remember media that fails to open instead of
  reopening and warning on every frame
- Close decoders of media no clip uses any more, and give two clips of one file their own decoders
- Skip the GPU pass for a single opaque layer of the sequence's size, and read back without waiting
  on the GPU in the same job
- Decode a frame's layers in parallel, and keep cache hits out of the render latency average
- Release replaced frames from the atlas when they are replaced, not on the next paint
- Zoom, a 100 % view and safe-area overlays

## Timeline editing

- Drop the selection when its clip goes away through undo, redo or another edit, and clamp the view
  when the timeline shrinks
- Commit a trim only on the lane that previewed it, keeping the grab offset: releasing over another
  track commits the stale preview, over the ruler discards it, and the edge jumps on the first move
- Keep snapped starts on the frame grid; clip ends come from probed durations, so snapping an end
  leaves the start between frames
- Lay out only the clips in view, instead of building every clip with its handles and label on each
  playhead tick
- Select several clips by shift-click, marquee and Ctrl+A
- Scroll while dragging near an edge, add scrollbars, and keep the grab point when the view scrolls
  mid-drag
- Insert and overwrite on drop, and say why a red drop was refused
- Copy, paste, cut and duplicate clips
- Go to the next and previous edit, set in and out points and markers, and cancel a drag with Escape
- Track headers with names, lock, mute, solo and a height, and an explanation on the disabled remove
  button
- Keep trim handles narrower than the clip, so narrow clips can still be dragged by the body

## Media bin and workspace

- A sequence settings dialog and a New Project action; every project starts as a hardcoded
  1920×1080, 30 fps "Untitled"
- Show an asset once it is probed and decode its thumbnail afterwards
- Bound the number of probes and thumbnail decodes running at once, and cancel them when the project
  is replaced
- Expand dropped folders, filter unsupported files in the dialog and on drop, and collapse failures
  into one dismissible row
- Remove assets, sort and search them, show their metadata and mark unused ones
- Release a thumbnail from the atlas when an import replaces it, and drop thumbnails of assets that
  undo removed
- Compare canonical paths when skipping duplicate imports
- Menus, a shortcut reference and preferences

## Infrastructure

- A CI workflow running the format check, clippy and the tests on every push, with the Vulkan tests
  on lavapipe or skipped without an adapter
- Test fixtures of small generated media files for decode and export tests: B-frames, a non-zero
  start time, 30000/1001 in a 1/1000 time base, stereo and planar audio, several audio streams,
  stills and audio-only files
- Tests for `Mixer::render`, the `Output` lifecycle, `PlaybackClock`, `ProjectEditor` undo and redo,
  the bin's import path and cross-track clip moves
- Bring the README up to date: it says Tessera can't import or play video, lists five of the seven
  crates and leaves out `rust-formatter`
- Correct CLAUDE.md, which has `tessera-render` depending on `tessera-timeline`

## Compositing

- Import hardware-decoded frames into the compositor without a copy, through dmabuf and Vulkan
  external memory
- Per-clip transform (position, scale, rotation, crop) and opacity
- Cross-dissolve and dip-to-colour transitions

## Export

- Encode the composited timeline with hardware encoders (VAAPI, NVENC, Vulkan Video), falling back
  to software
- Mux the mixed audio alongside the video
- An export dialog with presets for H.264, HEVC and AV1 in MP4 and MKV
- Run exports in the background with progress and cancellation

## Direction

- Keyframed clip properties with an easing curve editor
- Titles and text clips
- Effects written as `wgpu` shaders, with a common parameter model
- Proxy generation for codecs too heavy to edit natively
- Several sequences per project, and nesting one sequence inside another
- Colour management and an HDR pipeline end to end
- Share a GPU device between GPUI and the compositor once GPUI allows it, dropping the CPU readback
- Flatpak packaging
