## Decode

- A cache of recent frames for scrubbing, and decoding forward instead of seeking when the requested
  frame is shortly ahead of the last one
- Hardware decode through the discovered accelerators, VAAPI and Vulkan Video first, falling back to
  software
- Decode audio and resample it to a project-wide sample rate

## Timeline editing

- A time ruler with timecode and a playhead that can be clicked and dragged
- Zoom and horizontal scroll, replacing the fixed `PIXELS_PER_SECOND`
- Drag assets from the media bin onto a track
- Select, move and trim clips while keeping the no-overlap invariant
- Split at the playhead, delete and ripple delete
- Add, remove and reorder tracks
- Snapping to clip edges and the playhead
- Undo and redo as a command history over `Project`

## Playback

- Show the frame under the playhead in the viewer
- Play, pause, JKL shuttle and frame stepping
- Audio output through PipeWire, with the audio clock driving A/V sync
- Drop frames rather than drift when decode or compositing falls behind

## Compositing

- Upload decoded frames to `wgpu` textures and composite the video tracks onto a sequence-sized
  target
- Convert YUV to RGB with the source's matrix and range
- Fit sources whose resolution or aspect ratio differs from the sequence
- Wire the `Compositor` into the viewer through the CPU readback path
- Import hardware-decoded frames into the compositor without a copy, through dmabuf and Vulkan
  external memory
- Per-clip transform (position, scale, rotation, crop) and opacity
- Cross-dissolve and dip-to-colour transitions

## Audio

- Mix the audio tracks, with per-clip gain and per-track volume, mute and solo
- Draw waveforms on audio clips

## Project files

- Save and open projects in a versioned on-disk format, with migrations between versions
- Save As, recent projects and a dirty indicator in the title bar
- Relink media that has moved or gone missing
- Autosave and recovery after a crash

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

## Infrastructure

- A CI workflow running the format check, clippy and the tests on every push
- Test fixtures of small generated media files for decode and export tests
