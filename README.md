# glide

Polished screen recordings for Linux. Record a window and glide turns it into a
4K video with the window centered on your wallpaper, rounded corners, a smooth
redrawn cursor, and an optional camera that zooms in and follows your cursor.

It is inspired by macOS tools like Screen Studio and built for KDE Plasma on
Wayland.

## Features

- **Window or screen recording** through the desktop's ScreenCast portal, so
  you pick what to share in the system dialog.
- **Centered on your wallpaper.** The finished video places the window in the
  middle of your current KDE wallpaper (or a gradient, or any image) with
  rounded corners.
- **Cursor-following zoom (opt-in).** With `--zoom`, the camera looks ahead
  and zooms into where you are working, then eases back out when the cursor
  roams or rests.
- **Smooth cursor.** The real cursor is hidden while recording and redrawn
  afterwards as a crisp, smoothed arrow at any zoom level.
- **4K output, rendered on the GPU.** Compositing runs in a wgpu compute
  shader and encoding uses VAAPI hardware H.264, falling back to x264 when
  VAAPI is unavailable.
- **Liquid Glass toolbar.** `glide ui` shows a draggable floating toolbar
  with real KWin background blur, a liquid selection highlight and glossy
  buttons.

## Requirements

- Linux with a Wayland session. Developed on KDE Plasma 6 (KWin).
- PipeWire and `xdg-desktop-portal` with a ScreenCast implementation
  (`xdg-desktop-portal-kde` on Plasma).
- `ffmpeg` and `ffprobe` on `PATH`. VAAPI support is used when available.
- A GPU with Vulkan (or OpenGL ES) drivers.
- Rust 1.95 or newer, plus the PipeWire development headers and `clang` to
  build the PipeWire bindings.

On Arch Linux:

```sh
sudo pacman -S --needed rustup clang pipewire ffmpeg xdg-desktop-portal-kde vulkan-icd-loader
```

## Build

```sh
cargo build --release
```

The binary is `target/release/glide`.

## Usage

### Toolbar

```sh
glide ui
```

A glass toolbar appears near the bottom of the screen. Drag it anywhere.

1. Choose **Window** or **Screen**, a **Zoom** level and a **Background**.
2. Press **Record** and pick what to record in the system dialog.
3. Press **■** in the recording pill to stop.
4. glide renders the video and shows **Open** and **Show in folder**.

Press **Esc** or **✕** to close the toolbar.

Finished videos are saved to `~/Videos/glide/` (your XDG videos directory).
Raw recordings are kept in `~/.cache/glide/` so they can be rendered again.

#### Keyboard shortcut

In KDE System Settings, open **Keyboard → Shortcuts → Add New → Command or
Script**, use the full path to `glide ui` as the command, and assign a shortcut
such as **Meta+Shift+R**.

### Command line

Record a window into a directory. Press **Ctrl+C** to stop:

```sh
glide record myvideo
```

Render it:

```sh
glide render myvideo -o myvideo.mp4
glide render myvideo -o myvideo.mp4 --zoom
glide render myvideo -o myvideo.mp4 --zoom 2.5
glide render myvideo -o myvideo.mp4 --background ~/Pictures/backdrop.jpg
```

| Option | Meaning |
| --- | --- |
| `-o, --output <file>` | Output video (default `glide.mp4`). |
| `--zoom [factor]` | Follow the cursor with zoom, up to `factor` (1 to 4, default 1.8). Without it the video is not zoomed. |
| `--background <image>` | Use an image behind the window instead of your KDE wallpaper. |

## How it works

```
record:  portal ─► PipeWire frames ─► lossless screen.mkv
                   cursor metadata ─► cursor.txt (one position per frame)

render:  ffmpeg decode ─► GPU compute shader ─► NV12 ─► VAAPI H.264 ─► mp4
                          (wallpaper, rounded window, camera, cursor)
```

- `src/record.rs` talks to the ScreenCast portal and PipeWire. The cursor is
  requested as metadata, so frames are recorded without it and its position
  is logged separately.
- `src/camera.rs` plans the camera offline with look-ahead and smooths every
  move with critically damped springs.
- `src/composite.wgsl` and `src/gpu.rs` composite each frame on the GPU and
  write NV12 directly for the hardware encoder.
- `src/render.rs` pipelines decoding, compositing and encoding on separate
  threads.
- `src/ui/` is the toolbar: a full-screen click-through layer-shell surface
  drawn with egui and wgpu, KWin blur behind the glass, and background jobs
  for recording and rendering.

## Limitations

- Only tested on KDE Plasma 6 (Wayland). The toolbar needs layer-shell
  support, and its blur uses a KDE protocol.
- Videos are always 3840×2160 at 60 fps. A 1080p window is upscaled, so the
  window's own text gains no extra detail.
- Resizing a window while recording crops it to its starting size.
- When recording the whole screen, the toolbar's stop pill is visible in the
  recording.

## Development

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

`design/toolbar-preview.html` is an interactive mockup of the toolbar styles.
Open it in a browser, optionally with `?wallpaper=file:///path/to/image.jpg`.
