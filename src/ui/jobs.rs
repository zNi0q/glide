use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::Sender,
    thread,
};

use anyhow::{Context, Result};
use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

use crate::{
    background::Background,
    record::{self, Cancelled, Source, StopHandle},
    render,
};

pub enum Event {
    RecordingStarted,
    RecordingFinished(Result<PathBuf>),
    RenderProgress(usize, usize),
    RenderFinished(Result<PathBuf>),
    ScreenshotFinished(Result<PathBuf>),
    BackgroundChosen(Option<PathBuf>),
}

pub fn start_recording(source: Source, events: Sender<Event>) -> StopHandle {
    let (stop, signal) = record::stop_channel();
    thread::spawn(move || {
        let started = events.clone();
        let result = raw_dir().and_then(|dir| {
            record::record(&dir, source, signal, move || {
                let _ = started.send(Event::RecordingStarted);
            })?;
            Ok(dir)
        });
        let _ = events.send(Event::RecordingFinished(result));
    });
    stop
}

pub fn start_render(dir: PathBuf, zoom: f32, background: Background, events: Sender<Event>) {
    thread::spawn(move || {
        let progress = events.clone();
        let result = output_path(&dir).and_then(|output| {
            render::run(&dir, &output, zoom, &background, |done, total| {
                let _ = progress.send(Event::RenderProgress(done, total));
            })?;
            Ok(output)
        });
        let _ = events.send(Event::RenderFinished(result));
    });
}

pub fn take_screenshot(source: Source, background: Background, events: Sender<Event>) {
    thread::spawn(move || {
        let result = record::screenshot(source).and_then(|image| {
            let output = output_file("PICTURES", "Pictures", &stamp(), "png")?;
            render::still(&image, &output, &background)?;
            Ok(output)
        });
        let _ = events.send(Event::ScreenshotFinished(result));
    });
}

pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Cancelled>().is_some()
}

pub fn choose_background(events: Sender<Event>) {
    thread::spawn(move || {
        let chosen = tokio::runtime::Runtime::new()
            .ok()
            .and_then(|runtime| runtime.block_on(pick_image()).ok().flatten());
        let _ = events.send(Event::BackgroundChosen(chosen));
    });
}

pub fn open(path: &Path) {
    let _ = Command::new("xdg-open")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

async fn pick_image() -> Result<Option<PathBuf>> {
    let files = SelectedFiles::open_file()
        .title("Choose a background")
        .modal(true)
        .filter(FileFilter::new("Images").mimetype("image/*"))
        .send()
        .await?
        .response()?;
    Ok(files
        .uris()
        .first()
        .and_then(|uri| uri.as_str().strip_prefix("file://"))
        .map(|path| PathBuf::from(percent_decode(path))))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn raw_dir() -> Result<PathBuf> {
    let cache = match env::var_os("XDG_CACHE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".cache"),
    };
    Ok(cache.join("glide").join(stamp()))
}

fn stamp() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d_%H-%M-%S").to_string()
}

fn output_path(raw_dir: &Path) -> Result<PathBuf> {
    let name = raw_dir
        .file_name()
        .context("recording directory has no name")?
        .to_string_lossy();
    output_file("VIDEOS", "Videos", &name, "mp4")
}

fn output_file(xdg_dir: &str, fallback: &str, name: &str, extension: &str) -> Result<PathBuf> {
    let xdg = Command::new("xdg-user-dir")
        .arg(xdg_dir)
        .output()
        .ok()
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        .filter(|dir| dir.is_absolute() && Some(dir.as_path()) != home().ok().as_deref());
    let dir = match xdg {
        Some(dir) => dir,
        None => home()?.join(fallback),
    }
    .join("glide");
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir.join(format!("glide-{name}.{extension}")))
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_file_uri_paths() {
        assert_eq!(
            percent_decode("/home/a/My%20Pics/x.png"),
            "/home/a/My Pics/x.png"
        );
        assert_eq!(percent_decode("/caf%C3%A9.jpg"), "/café.jpg");
        assert_eq!(percent_decode("/100%"), "/100%");
    }
}
