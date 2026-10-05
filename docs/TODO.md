## Model

- Link the video and audio clips placed from one asset, so one drop places both and move, trim,
  split, delete and ripple delete keep them in sync (`ripple_delete_clip` shifts only its own track)
- Carry rotation, sample aspect ratio, pixel format, stream disposition and per-stream start time in
  `MediaInfo`, and let a clip choose which of several audio streams it plays

## Project files

- Autosave and recovery after a crash

## Media decode

- Apply the display-matrix rotation and the sample aspect ratio, so phone footage isn't sideways and
  anamorphic DV and HDV aren't squashed
- Re-anchor audio on packet timestamps after gaps and jumps, not only after a seek
- Keep a decoded run of frames for backward stepping and reverse shuttle, and budget frame caches
  across decoders instead of 256 MiB each
- Deinterlace interlaced sources, and interpolate chroma fully in the scaler

## Playback and audio

- Keep one PipeWire connection for the session instead of connecting on every play, blocking the UI
  thread on it, and joining the stream thread on every pause
- Play the sound of video clips, through linked audio clips or their own stream
- Mix the audio tracks, with per-clip gain and per-track volume
- Play sound at shuttle speeds, in reverse and while scrubbing or stepping
- Retry media that failed during playback, and open decoders ahead of clip starts with reused mix
  buffers instead of on the feeder thread at the first block
- Draw waveforms on audio clips

## Viewer

- Read the composited frame back without waiting on the GPU in the same job
- Zoom and a 100 % view

## Timeline editing

- Scroll while dragging near an edge, add scrollbars, and keep the grab point when the view scrolls
  mid-drag

## Media bin and workspace

- Menus, a shortcut reference and preferences

## Infrastructure

- A CI workflow running the format check, clippy and the tests on every push, with the Vulkan tests
  on lavapipe or skipped without an adapter
- Test fixtures of small generated media files for decode and export tests: B-frames, 30000/1001 in
  a 1/1000 time base, stereo and planar audio, several audio streams, stills and audio-only files
- Tests for `Mixer::render`, the `Output` lifecycle, `PlaybackClock`, `ProjectEditor` undo and redo,
  the bin's import path and cross-track clip moves

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
