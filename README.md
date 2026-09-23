# Tessera

A video editor for Linux, written in Rust with a [GPUI](https://www.gpui.rs) interface and
hardware-accelerated decode, render and encode.

> Tessera is in early development. The project model, media probing and the main window are in
> place, but it cannot import, play back or export video yet.

## Features

- A timeline model that keeps time as integer flicks (1/705 600 000 s), so frame and time
  conversions are exact at every common frame rate, the NTSC 1000/1001 rates included.
- Media probing and hardware-acceleration discovery (VAAPI, Vulkan, CUDA, QSV, DRM) through the
  system FFmpeg.
- A Vulkan compositor built on `wgpu`.
- A workspace with a media bin, a viewer and a multi-track timeline.

## Requirements

- Linux
- The latest stable Rust toolchain
- FFmpeg development headers
- Clang, for bindgen to generate the FFmpeg bindings
- A Vulkan driver

## Building and running

```sh
cargo build
cargo run
```

Logging is at `info` by default. Set `RUST_LOG` to change it, for example
`RUST_LOG=debug cargo run`. Press <kbd>Ctrl</kbd>+<kbd>Q</kbd> to quit.

## Workspace layout

| Crate              | Purpose                                                        |
| ------------------ | -------------------------------------------------------------- |
| `tessera-timeline` | The project model: projects, timelines, tracks, clips, assets. No IO, no GPU. |
| `tessera-media`    | Everything that touches FFmpeg: probing, hwaccel discovery, decode and encode. |
| `tessera-render`   | The `wgpu` Vulkan compositor.                                  |
| `tessera-ui`       | The GPUI views: workspace, media bin, viewer and timeline.     |
| `tessera`          | The application binary.                                        |

## Development

```sh
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

[MIT](LICENSE)
