use std::{
    fs,
    io::{ErrorKind, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
};

use anyhow::{Context, Result, bail};

use crate::{
    CURSOR_FILE, FPS, SCREEN_FILE,
    background::{self, Background},
    camera::{self, Camera, Point},
    gpu::{Compositor, Output, Params},
    record::Image,
};

const SCENE_W: u32 = 1920;
const SCENE_H: u32 = 1080;
const OUT_W: u32 = 3840;
const OUT_H: u32 = 2160;
const MARGIN: f32 = 0.08;
const VAAPI_DEVICE: &str = "/dev/dri/renderD128";

struct Layout {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    scale: f32,
}

impl Layout {
    fn new(win_w: u32, win_h: u32) -> Self {
        let fit = 1.0 - 2.0 * MARGIN;
        let scale = (fit * SCENE_W as f32 / win_w as f32).min(fit * SCENE_H as f32 / win_h as f32);
        let (width, height) = (win_w as f32 * scale, win_h as f32 * scale);
        Self {
            x: (SCENE_W as f32 - width) / 2.0,
            y: (SCENE_H as f32 - height) / 2.0,
            width,
            height,
            scale,
        }
    }

    fn to_scene(&self, (x, y): Point) -> Point {
        (self.x + x * self.scale, self.y + y * self.scale)
    }
}

pub struct RenderOptions {
    pub zoom: f32,
    pub background: Background,
    pub end_frame: Option<usize>,
}

pub fn run(
    dir: &Path,
    output: &Path,
    options: &RenderOptions,
    mut progress: impl FnMut(usize, usize),
) -> Result<()> {
    let screen = dir.join(SCREEN_FILE);
    let (win_w, win_h) = probe_size(&screen)?;
    let layout = Layout::new(win_w, win_h);
    let mut cursor: Vec<Option<Point>> = read_cursor_log(&dir.join(CURSOR_FILE))?
        .into_iter()
        .map(|p| p.map(|p| layout.to_scene(p)))
        .collect();
    let end_frame = options.end_frame.unwrap_or(usize::MAX);
    cursor.truncate(end_frame);
    let cameras = camera::plan(&cursor, SCENE_W as f32, SCENE_H as f32, options.zoom, FPS);
    let cursors = camera::smooth_cursor(&cursor, FPS);
    let total_frames = cursor.len();
    let background = background::load(&options.background, OUT_W, OUT_H)?;
    let compositor = Compositor::new((win_w, win_h), (OUT_W, OUT_H), &background, Output::Nv12)?;
    drop(background);

    let mut decoder = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(&screen)
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("cannot start ffmpeg (is it installed?)")?;
    let mut encoder = encoder_command(output)
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot start ffmpeg (is it installed?)")?;
    let mut frames_in = decoder.stdout.take().expect("stdout is piped");
    let mut frames_out = encoder.stdin.take().expect("stdin is piped");
    let (frame_tx, frame_rx) = mpsc::sync_channel::<Vec<u8>>(2);
    let (free_tx, free_rx) = mpsc::channel();
    let writer = thread::spawn(move || -> Result<()> {
        for frame in frame_rx {
            frames_out
                .write_all(&frame)
                .context("ffmpeg stopped accepting frames")?;
            let _ = free_tx.send(frame);
        }
        Ok(())
    });

    let window_len = win_w as usize * win_h as usize * 4;
    let (decoded_tx, decoded_rx) = mpsc::sync_channel::<Vec<u8>>(2);
    let (spare_tx, spare_rx) = mpsc::channel();
    let reader = thread::spawn(move || -> Result<()> {
        loop {
            let mut window = spare_rx.try_recv().unwrap_or_else(|_| vec![0; window_len]);
            match frames_in.read_exact(&mut window) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e).context("cannot read decoded frames"),
            }
            if decoded_tx.send(window).is_err() {
                return Ok(());
            }
        }
    });

    let overview = overview();
    let mut rendered = 0;
    for window in decoded_rx.iter().take(end_frame) {
        let cam = cameras.get(rendered).unwrap_or(&overview);
        let params = frame_params(&layout, cam, cursors.get(rendered).copied().flatten());
        let mut nv12 = free_rx.try_recv().unwrap_or_default();
        compositor.render(&window, &params, &mut nv12)?;
        let _ = spare_tx.send(window);
        if frame_tx.send(nv12).is_err() {
            break;
        }
        rendered += 1;
        progress(rendered, total_frames.max(rendered));
    }

    drop(decoded_rx);
    drop(frame_tx);
    reader.join().expect("reader thread panicked")?;
    writer.join().expect("writer thread panicked")?;
    let _ = decoder.wait();
    let status = encoder.wait()?;
    if rendered == 0 {
        bail!("no frames could be decoded from {}", screen.display());
    }
    if !status.success() {
        bail!("ffmpeg failed with {status}");
    }
    Ok(())
}

pub fn still(image: &Image, output: &Path, background: &Background) -> Result<()> {
    let layout = Layout::new(image.width, image.height);
    let background = background::load(background, OUT_W, OUT_H)?;
    let compositor = Compositor::new(
        (image.width, image.height),
        (OUT_W, OUT_H),
        &background,
        Output::Rgba,
    )?;
    let mut rgba = Vec::new();
    compositor.render(
        &image.rgba,
        &frame_params(&layout, &overview(), None),
        &mut rgba,
    )?;

    let mut encoder = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .args(["-video_size", &format!("{OUT_W}x{OUT_H}"), "-i", "-"])
        .args(["-frames:v", "1"])
        .arg(output)
        .stdin(Stdio::piped())
        .spawn()
        .context("cannot start ffmpeg (is it installed?)")?;
    encoder
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(&rgba)
        .context("ffmpeg stopped accepting the image")?;
    let status = encoder.wait()?;
    if !status.success() {
        bail!("ffmpeg failed with {status}");
    }
    Ok(())
}

fn overview() -> Camera {
    Camera {
        x: SCENE_W as f32 / 2.0,
        y: SCENE_H as f32 / 2.0,
        zoom: 1.0,
    }
}

fn frame_params(layout: &Layout, cam: &Camera, cursor: Option<Point>) -> Params {
    Params {
        camera: [cam.x, cam.y, cam.zoom, 0.0],
        window_rect: [layout.x, layout.y, layout.width, layout.height],
        cursor: cursor.map_or([0.0; 4], |(x, y)| [x, y, 1.0, 0.0]),
        sizes: [OUT_W as f32, OUT_H as f32, SCENE_W as f32, SCENE_H as f32],
    }
}

fn encoder_command(output: &Path) -> Command {
    let vaapi = vaapi_available();
    eprintln!(
        "Encoding with {}",
        if vaapi {
            "VAAPI (GPU)"
        } else {
            "libx264 (CPU)"
        }
    );
    let mut command = Command::new("ffmpeg");
    command.args(["-loglevel", "error", "-y"]);
    if vaapi {
        command.args(["-vaapi_device", VAAPI_DEVICE]);
    }
    command
        .args(["-f", "rawvideo", "-pix_fmt", "nv12"])
        .args(["-video_size", &format!("{OUT_W}x{OUT_H}")])
        .args(["-framerate", &FPS.to_string(), "-i", "-"]);
    let color = "setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709";
    if vaapi {
        command.args(["-vf", &format!("{color},hwupload")]);
        command.args(["-c:v", "h264_vaapi", "-qp", "18"]);
    } else {
        command.args(["-vf", color]);
        command.args(["-c:v", "libx264", "-preset", "medium", "-crf", "18"]);
    }
    command.args(["-movflags", "+faststart"]).arg(output);
    command
}

fn vaapi_available() -> bool {
    Command::new("ffmpeg")
        .args(["-loglevel", "quiet", "-vaapi_device", VAAPI_DEVICE])
        .args(["-f", "lavfi", "-i", "color=size=256x256:duration=0.1"])
        .args([
            "-vf",
            "format=nv12,hwupload",
            "-c:v",
            "h264_vaapi",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn probe_size(video: &Path) -> Result<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0:s=x"])
        .arg(video)
        .output()
        .context("cannot run ffprobe (is ffmpeg installed?)")?;
    if !out.status.success() {
        bail!(
            "cannot read {}: {}",
            video.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (w, h) = text
        .trim()
        .split_once('x')
        .context("unexpected ffprobe output")?;
    Ok((w.parse()?, h.parse()?))
}

fn read_cursor_log(path: &Path) -> Result<Vec<Option<Point>>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            if line == "-" {
                return Ok(None);
            }
            let parsed = line
                .split_once(' ')
                .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)));
            parsed
                .map(Some)
                .with_context(|| format!("{}:{}: invalid cursor position", path.display(), i + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a GPU and ffmpeg"]
    fn still_renders_a_4k_png() {
        let (width, height) = (320, 200);
        let rgba = (0..width * height)
            .flat_map(|i| [(i % width) as u8, 120, 200, 255])
            .collect();
        let image = Image {
            width,
            height,
            rgba,
        };
        let output = std::env::temp_dir().join("glide-still-test.png");
        still(&image, &output, &Background::Gradient).unwrap();
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height,codec_name",
            ])
            .args(["-of", "csv=p=0"])
            .arg(&output)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&probe.stdout).trim(),
            "png,3840,2160"
        );
        fs::remove_file(output).unwrap();
    }

    #[test]
    #[ignore = "needs a GPU and ffmpeg"]
    fn render_stops_at_the_end_frame() {
        let dir = std::env::temp_dir().join("glide-end-frame-test");
        fs::create_dir_all(&dir).unwrap();
        let generated = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=320x200:rate=60:duration=1"])
            .args(["-c:v", "libx264rgb", "-crf", "0"])
            .arg(dir.join(SCREEN_FILE))
            .status()
            .unwrap();
        assert!(generated.success());
        fs::write(dir.join(CURSOR_FILE), "10 10\n".repeat(60)).unwrap();
        let output = dir.join("out.mp4");
        let options = RenderOptions {
            zoom: 1.0,
            background: Background::Gradient,
            end_frame: Some(24),
        };
        run(&dir, &output, &options, |_, _| {}).unwrap();
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=nb_read_frames",
            ])
            .args(["-of", "csv=p=0"])
            .arg(&output)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "24");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn layout_centers_window_with_margin() {
        let layout = Layout::new(1280, 800);
        assert!((layout.x * 2.0 + layout.width - SCENE_W as f32).abs() < 1e-3);
        assert!((layout.y * 2.0 + layout.height - SCENE_H as f32).abs() < 1e-3);
        assert!((layout.height - SCENE_H as f32 * (1.0 - 2.0 * MARGIN)).abs() < 1e-3);
    }
}
