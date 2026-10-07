use std::{
    fmt,
    fs::File,
    io::{BufWriter, Write},
    os::{fd::OwnedFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use ashpd::{
    desktop::{
        PersistMode, ResponseError,
        screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    },
    enumflags2::BitFlags,
};
use nix::sys::signal::{SIGINT, SigSet};
use pipewire::{
    self as pw,
    properties::properties,
    spa::{
        self,
        buffer::{ChunkFlags, meta::MetaCursor},
        param::{
            ParamType,
            format::{FormatProperties, MediaSubtype, MediaType},
            video::{VideoFormat, VideoInfoRaw},
        },
        pod::{ChoiceValue, Object, Pod, Property, Value, serialize::PodSerializer},
        utils::{Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes},
    },
    stream::{StreamBox, StreamFlags, StreamState},
};

use crate::{CLICKS_FILE, CURSOR_FILE, FPS, SCREEN_FILE, clicks, x11};

#[derive(Default)]
pub(crate) struct Latest {
    pub(crate) frame: Option<Frame>,
    pub(crate) cursor: Option<(f32, f32)>,
}

pub(crate) struct Frame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) pix_fmt: &'static str,
    pub(crate) data: Vec<u8>,
}

impl Frame {
    pub(crate) fn into_image(mut self) -> Image {
        let swap_red_blue = self.pix_fmt.starts_with("bgr");
        for pixel in self.data.as_chunks_mut::<4>().0 {
            if swap_red_blue {
                pixel.swap(0, 2);
            }
            pixel[3] = 255;
        }
        Image {
            width: self.width as u32,
            height: self.height as u32,
            rgba: self.data,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Window,
    Screen,
}

pub const NO_CLICK_ACCESS: &str = "Clicks were not recorded: glide cannot read the mouse. Run `sudo usermod -aG input $USER`, then log out and back in.";
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const RECORDING_CURSOR: [CursorMode; 3] = [
    CursorMode::Metadata,
    CursorMode::Embedded,
    CursorMode::Hidden,
];
const SCREENSHOT_CURSOR: [CursorMode; 3] = [
    CursorMode::Hidden,
    CursorMode::Metadata,
    CursorMode::Embedded,
];

#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("selection was cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Recording {
    pub frames: u32,
    pub cursor_frames: u32,
    pub clicks: Option<usize>,
}

#[derive(Clone)]
pub struct StopHandle {
    sender: pw::channel::Sender<()>,
    stopped: Arc<AtomicBool>,
}

pub struct StopSignal {
    receiver: pw::channel::Receiver<()>,
    stopped: Arc<AtomicBool>,
}

impl StopHandle {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.sender.send(());
    }
}

impl StopSignal {
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

pub fn stop_channel() -> (StopHandle, StopSignal) {
    let (sender, receiver) = pw::channel::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    (
        StopHandle {
            sender,
            stopped: Arc::clone(&stopped),
        },
        StopSignal { receiver, stopped },
    )
}

pub fn run_cli(dir: &Path) -> Result<()> {
    let sigint = SigSet::from_iter([SIGINT]);
    sigint.thread_block()?;
    let (stop, signal) = stop_channel();
    thread::spawn(move || {
        if sigint.wait().is_ok() {
            stop.stop();
        }
    });

    let recording = record(dir, Source::Window, signal, || {
        println!("Recording... press Ctrl+C to stop.");
    })?;
    println!(
        "Saved {} frames ({:.1}s) to {}",
        recording.frames,
        recording.frames as f32 / FPS as f32,
        dir.display()
    );
    if recording.cursor_frames == 0 {
        eprintln!(
            "warning: the compositor reported no cursor positions, so no cursor will be drawn"
        );
    }
    match recording.clicks {
        Some(count) => println!("Recorded {count} clicks"),
        None => eprintln!("{NO_CLICK_ACCESS}"),
    }
    Ok(())
}

pub fn record(
    dir: &Path,
    source: Source,
    stop: StopSignal,
    on_start: impl FnOnce(),
) -> Result<Recording> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    if x11::is_session() {
        record_x11(dir, source, stop, on_start)
    } else {
        with_stream(source, &RECORDING_CURSOR, |node_id, fd| {
            on_start();
            capture(dir, clicks::listen(), |latest| {
                stream_window(node_id, fd, latest, stop)
            })
        })
    }
}

fn record_x11(
    dir: &Path,
    source: Source,
    stop: StopSignal,
    on_start: impl FnOnce(),
) -> Result<Recording> {
    let target = x11::Target::choose(source)?;
    on_start();
    capture(dir, x11::listen_clicks(), |latest| {
        target.stream(&latest, &stop)
    })
}

pub fn screenshot(source: Source) -> Result<Image> {
    if x11::is_session() {
        return x11::Target::choose(source)?.snapshot();
    }
    with_stream(source, &SCREENSHOT_CURSOR, |node_id, fd| {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let (stop, signal) = stop_channel();
        let watcher = {
            let latest = Arc::clone(&latest);
            thread::spawn(move || {
                let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
                while Instant::now() < deadline
                    && latest
                        .lock()
                        .expect("capture state poisoned")
                        .frame
                        .is_none()
                {
                    thread::sleep(Duration::from_millis(5));
                }
                stop.stop();
            })
        };
        let streamed = stream_window(node_id, fd, Arc::clone(&latest), signal);
        let _ = watcher.join();
        streamed?;
        let frame = latest
            .lock()
            .expect("capture state poisoned")
            .frame
            .take()
            .context("no frame arrived from the compositor")?;
        Ok(frame.into_image())
    })
}

pub fn portal_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("cannot start the async runtime"))
}

fn with_stream<T>(
    source: Source,
    cursor_preference: &[CursorMode],
    use_stream: impl FnOnce(u32, OwnedFd) -> Result<T>,
) -> Result<T> {
    let runtime = portal_runtime();
    let (proxy, session, node_id, fd) = runtime.block_on(open_portal(source, cursor_preference))?;
    let result = use_stream(node_id, fd);
    let _ = runtime.block_on(session.close());
    drop(proxy);
    result
}

async fn open_portal(
    source: Source,
    cursor_preference: &[CursorMode],
) -> Result<(
    Screencast,
    ashpd::desktop::Session<Screencast>,
    u32,
    OwnedFd,
)> {
    let proxy = Screencast::new()
        .await
        .context("the ScreenCast portal is unavailable")?;
    let source_type = match source {
        Source::Window => SourceType::Window,
        Source::Screen => SourceType::Monitor,
    };
    if !proxy.available_source_types().await?.contains(source_type) {
        bail!(match source {
            Source::Window => "this desktop cannot share single windows, choose Screen instead",
            Source::Screen => "this desktop cannot share whole screens, choose Window instead",
        });
    }
    let cursor_modes = proxy.available_cursor_modes().await.unwrap_or_default();
    let cursor_mode = cursor_preference
        .iter()
        .copied()
        .find(|mode| cursor_modes.contains(*mode));

    let session = proxy.create_session(Default::default()).await?;
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor_mode)
                .set_sources(BitFlags::from(source_type))
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await?;
    let streams = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .map_err(|e| match e {
            ashpd::Error::Response(ResponseError::Cancelled) => anyhow::Error::new(Cancelled),
            other => anyhow::Error::new(other),
        })?;
    let node_id = streams
        .streams()
        .first()
        .context("nothing was selected")?
        .pipe_wire_node_id();
    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await?;
    Ok((proxy, session, node_id, fd))
}

fn capture(
    dir: &Path,
    listener: Option<clicks::ClickListener>,
    produce: impl FnOnce(Arc<Mutex<Latest>>) -> Result<()>,
) -> Result<Recording> {
    let latest = Arc::new(Mutex::new(Latest::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (dir, latest, stop) = (dir.to_owned(), Arc::clone(&latest), Arc::clone(&stop));
        thread::spawn(move || write_frames(&dir, &latest, &stop))
    };

    let streamed = produce(latest);
    stop.store(true, Ordering::Relaxed);
    let (frames, cursor_frames, start) = writer.join().expect("writer thread panicked")?;
    let click_times = listener.map(clicks::ClickListener::finish);
    streamed?;

    if frames == 0 {
        bail!("no frames were captured");
    }
    let clicks = match (click_times, start) {
        (Some(times), Some(start)) => Some(write_click_log(dir, start, &times)?),
        _ => None,
    };
    Ok(Recording {
        frames,
        cursor_frames,
        clicks,
    })
}

pub fn write_click_log(dir: &Path, start: Instant, times: &[Instant]) -> Result<usize> {
    let frames: Vec<String> = times
        .iter()
        .filter_map(|time| time.checked_duration_since(start))
        .map(|offset| ((offset.as_secs_f64() * f64::from(FPS)) as u64).to_string())
        .collect();
    std::fs::write(dir.join(CLICKS_FILE), frames.join("\n") + "\n")?;
    Ok(frames.len())
}

fn stream_window(
    node_id: u32,
    fd: OwnedFd,
    latest: Arc<Mutex<Latest>>,
    stop_signal: StopSignal,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let stream = StreamBox::new(
        &core,
        "glide",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let _stop = stop_signal.receiver.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });

    let _listener = stream
        .add_local_listener_with_user_data(VideoInfoRaw::default())
        .state_changed({
            let mainloop = mainloop.clone();
            move |_, _, _, new| match new {
                StreamState::Error(e) => {
                    eprintln!("stream error: {e}");
                    mainloop.quit();
                }
                StreamState::Unconnected => mainloop.quit(),
                _ => {}
            }
        })
        .param_changed(|stream, format, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() || format.parse(param).is_err() {
                return;
            }
            let meta = cursor_meta_param();
            let pod = Pod::from_bytes(&meta).expect("serialized pod is valid");
            if let Err(e) = stream.update_params(&mut [pod]) {
                eprintln!("cannot request cursor metadata: {e}");
            }
        })
        .process(move |stream, format| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                store_buffer(&mut buffer, format, &latest);
            }
        })
        .register()?;

    let formats = format_param();
    stream.connect(
        Direction::Input,
        Some(node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&formats).expect("serialized pod is valid")],
    )?;

    mainloop.run();
    Ok(())
}

fn store_buffer(
    buffer: &mut pw::buffer::Buffer<'_>,
    format: &VideoInfoRaw,
    latest: &Mutex<Latest>,
) {
    let mut latest = latest.lock().expect("capture state poisoned");

    if let Some(cursor) = buffer.find_meta::<MetaCursor>() {
        let pos = cursor.position();
        latest.cursor = cursor.is_valid().then_some((pos.x as f32, pos.y as f32));
    }

    let Some(pix_fmt) = ffmpeg_pix_fmt(format.format()) else {
        return;
    };
    let (width, height) = (format.size().width as usize, format.size().height as usize);
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let (size, flags, offset, stride) = {
        let chunk = data.chunk();
        (
            chunk.size(),
            chunk.flags(),
            chunk.offset() as usize,
            chunk.stride(),
        )
    };
    if size == 0 || flags.contains(ChunkFlags::CORRUPTED) {
        return;
    }
    let Some(src) = data.data() else { return };

    let row_bytes = width * 4;
    let stride = usize::try_from(stride)
        .ok()
        .filter(|&s| s >= row_bytes)
        .unwrap_or(row_bytes);
    let frame = latest.frame.get_or_insert_with(|| Frame {
        width,
        height,
        pix_fmt,
        data: Vec::new(),
    });
    frame.width = width;
    frame.height = height;
    frame.pix_fmt = pix_fmt;
    frame.data.resize(row_bytes * height, 0);
    for (row, dst) in frame.data.chunks_exact_mut(row_bytes).enumerate() {
        let start = offset + row * stride;
        if let Some(line) = src.get(start..start + row_bytes) {
            dst.copy_from_slice(line);
        }
    }
}

fn write_frames(
    dir: &Path,
    latest: &Mutex<Latest>,
    stop: &AtomicBool,
) -> Result<(u32, u32, Option<Instant>)> {
    let (width, height, pix_fmt) = loop {
        if stop.load(Ordering::Relaxed) {
            return Ok((0, 0, None));
        }
        if let Some(f) = &latest.lock().expect("capture state poisoned").frame {
            break (f.width, f.height, f.pix_fmt);
        }
        thread::sleep(Duration::from_millis(5));
    };

    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
        ])
        .args(["-video_size", &format!("{width}x{height}")])
        .args(["-framerate", &FPS.to_string(), "-i", "-"])
        .args(["-c:v", "libx264rgb", "-preset", "ultrafast", "-crf", "0"])
        .arg(dir.join(SCREEN_FILE))
        .stdin(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("cannot start ffmpeg (is it installed?)")?;
    let mut stdin = ffmpeg.stdin.take().expect("stdin is piped");
    let mut cursor_log = BufWriter::new(File::create(dir.join(CURSOR_FILE))?);

    let mut image = vec![0; width * height * 4];
    let period = Duration::from_secs(1) / FPS;
    let start = Instant::now();
    let mut frames = 0;
    let mut cursor_frames = 0;
    while !stop.load(Ordering::Relaxed) {
        if let Some(wait) = (start + period * frames).checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        let cursor = {
            let latest = latest.lock().expect("capture state poisoned");
            if let Some(frame) = &latest.frame {
                copy_cropped(frame, &mut image, width, height);
            }
            latest.cursor
        };
        stdin
            .write_all(&image)
            .context("ffmpeg stopped accepting frames")?;
        match cursor {
            Some((x, y)) => {
                writeln!(cursor_log, "{x} {y}")?;
                cursor_frames += 1;
            }
            None => writeln!(cursor_log, "-")?,
        }
        frames += 1;
    }

    drop(stdin);
    cursor_log.flush()?;
    let status = ffmpeg.wait()?;
    if !status.success() {
        bail!("ffmpeg failed with {status}");
    }
    Ok((frames, cursor_frames, Some(start)))
}

fn copy_cropped(frame: &Frame, image: &mut [u8], width: usize, height: usize) {
    if frame.width == width && frame.height == height {
        image.copy_from_slice(&frame.data);
        return;
    }
    let row_bytes = width.min(frame.width) * 4;
    for row in 0..height.min(frame.height) {
        let src = row * frame.width * 4;
        let dst = row * width * 4;
        image[dst..dst + row_bytes].copy_from_slice(&frame.data[src..src + row_bytes]);
    }
}

fn ffmpeg_pix_fmt(format: VideoFormat) -> Option<&'static str> {
    match format {
        VideoFormat::BGRx => Some("bgr0"),
        VideoFormat::BGRA => Some("bgra"),
        VideoFormat::RGBx => Some("rgb0"),
        VideoFormat::RGBA => Some("rgba"),
        _ => None,
    }
}

fn format_param() -> Vec<u8> {
    serialize(spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 1920,
                height: 1080
            },
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: FPS, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
    ))
}

fn cursor_meta_size(bitmap_side: usize) -> i32 {
    let size = size_of::<spa::sys::spa_meta_cursor>()
        + size_of::<spa::sys::spa_meta_bitmap>()
        + bitmap_side * bitmap_side * 4;
    size as i32
}

fn cursor_meta_param() -> Vec<u8> {
    let size = ChoiceEnum::Range {
        default: cursor_meta_size(64),
        min: cursor_meta_size(1),
        max: cursor_meta_size(1024),
    };
    serialize(Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property::new(
                spa::sys::SPA_PARAM_META_type,
                Value::Id(Id(spa::sys::SPA_META_Cursor)),
            ),
            Property::new(
                spa::sys::SPA_PARAM_META_size,
                Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), size))),
            ),
        ],
    })
}

fn serialize(object: Object) -> Vec<u8> {
    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .expect("pod serialization into memory cannot fail")
        .0
        .into_inner()
}

#[cfg(test)]
mod tests {
    use super::{Frame, portal_runtime};

    #[test]
    #[ignore = "needs a desktop session with the ScreenCast portal"]
    fn portal_answers_repeated_requests() {
        for _ in 0..2 {
            let modes = portal_runtime().block_on(async {
                let query = async {
                    ashpd::desktop::screencast::Screencast::new()
                        .await?
                        .available_cursor_modes()
                        .await
                };
                tokio::time::timeout(std::time::Duration::from_secs(3), query).await
            });
            assert!(
                matches!(modes, Ok(Ok(_))),
                "portal did not answer: {modes:?}"
            );
        }
    }

    #[test]
    fn frames_become_opaque_rgba() {
        let frame = |pix_fmt| Frame {
            width: 2,
            height: 1,
            pix_fmt,
            data: vec![10, 20, 30, 0, 40, 50, 60, 7],
        };
        assert_eq!(
            frame("bgr0").into_image().rgba,
            [30, 20, 10, 255, 60, 50, 40, 255]
        );
        assert_eq!(
            frame("rgba").into_image().rgba,
            [10, 20, 30, 255, 40, 50, 60, 255]
        );
    }
}
