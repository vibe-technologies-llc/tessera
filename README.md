# Tessera

A video editor for Linux, written in Rust with a [GPUI](https://www.gpui.rs) interface and
hardware-accelerated decode, render and encode.

> Tessera is in early development. You can import media, edit it on a multi-track timeline, play it
> back with sound, and save and open projects, but it cannot export video yet.

## Features

- A timeline model that keeps time as integer flicks (1/705 600 000 s), so frame and time
  conversions are exact at every common frame rate, the NTSC 1000/1001 rates included, with
  undo and redo for every edit.
- Project files (`.tessera`): versioned JSON that is validated on load and written atomically, with
  media stored relative to the project where it sits under it.
- Media probing, hardware-accelerated decode (VAAPI, Vulkan Video) and audio decode through the
  system FFmpeg.
- Audio playback through PipeWire, which also clocks the picture.
- A Vulkan compositor built on `wgpu`.
- A workspace with a media bin, a viewer and a multi-track timeline: drag media in, move, trim,
  split and delete clips, select several at once, snap to edges and the playhead, and insert or
  overwrite on drop.

## Requirements

- Linux
- The latest stable Rust toolchain
- FFmpeg and PipeWire development headers
- Clang, for bindgen to generate the bindings
- A Vulkan driver

## Building and running

```sh
cargo build
cargo run
```

Logging is at `info` by default. Set `RUST_LOG` to change it, for example
`RUST_LOG=debug cargo run`. Press <kbd>Ctrl</kbd>+<kbd>Q</kbd> to quit.

## Workspace layout

| Crate              | Purpose                                                                           |
| ------------------ | --------------------------------------------------------------------------------- |
| `tessera-timeline` | The project model: projects, timelines, tracks, clips, assets, history. No IO, no GPU. |
| `tessera-document` | Reading and writing `.tessera` project files.                                     |
| `tessera-media`    | Everything that touches FFmpeg: probing, hwaccel discovery, video and audio decode. |
| `tessera-audio`    | Mixing the timeline's sound and playing it through PipeWire.                      |
| `tessera-render`   | The `wgpu` Vulkan compositor.                                                     |
| `tessera-ui`       | The GPUI views: workspace, media bin, viewer and timeline.                        |
| `tessera`          | The application binary.                                                           |

## Development

```sh
rust-formatter
cargo clippy --all-targets -- -D warnings
cargo test
```

Formatting goes through `rust-formatter`, a wrapper around nightly `rustfmt` that sets the import
grouping the code follows. `rust-formatter --check` only reports.

## License

[GNU Affero General Public License v3.0](LICENSE)
