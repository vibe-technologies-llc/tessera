## Playback and audio

- Keep one PipeWire connection for the session instead of connecting on every play, blocking the UI
  thread on it, and joining the stream thread on every pause
- Play sound at shuttle speeds, in reverse and while scrubbing or stepping
- Retry media that failed during playback, and open decoders ahead of clip starts with reused mix
  buffers instead of on the feeder thread at the first block
- Draw waveforms on audio clips

## Viewer

- Read the composited frame back without waiting on the GPU in the same job
- Zoom and a 100 % view

## Media bin and workspace

- Menus, a shortcut reference and preferences

## Infrastructure

- A CI workflow running the format check, clippy and the tests on every push, with the Vulkan tests
  on lavapipe or skipped without an adapter
- Tests for `Mixer::render`, the `Output` lifecycle, `PlaybackClock`, `ProjectEditor` undo and redo,
  the bin's import path and cross-track clip moves

## Compositing

- Import hardware-decoded frames into the compositor without a copy, through dmabuf and Vulkan
  external memory
- Cross-dissolve and dip-to-colour transitions

## Direction

- Keyframed clip properties with an easing curve editor
- Titles and text clips
- Effects written as `wgpu` shaders, with a common parameter model
- Proxy generation for codecs too heavy to edit natively
- Several sequences per project, and nesting one sequence inside another
- Colour management and an HDR pipeline end to end
- Share a GPU device between GPUI and the compositor once GPUI allows it, dropping the CPU readback
- Flatpak packaging
