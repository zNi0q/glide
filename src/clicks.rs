use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read},
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use nix::{
    fcntl::OFlag,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};

const EV_KEY: u16 = 1;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_TOUCH: u16 = 0x14a;
const EVENT_SIZE: usize = 24;
const TAP_MAX: Duration = Duration::from_millis(180);
const POLL_INTERVAL_MS: u16 = 100;

pub struct ClickListener {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Vec<Instant>>,
}

impl ClickListener {
    pub(crate) fn spawn(read: impl FnOnce(&AtomicBool) -> Vec<Instant> + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || read(&stop))
        };
        Self { stop, handle }
    }

    pub fn finish(self) -> Vec<Instant> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

pub fn available() -> bool {
    crate::x11::is_session() || !open_pointer_devices().is_empty()
}

pub fn listen() -> Option<ClickListener> {
    let devices = open_pointer_devices();
    if devices.is_empty() {
        return None;
    }
    Some(ClickListener::spawn(move |stop| read_clicks(devices, stop)))
}

fn open_pointer_devices() -> Vec<File> {
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("event"))
        .filter(|entry| {
            fs::read_to_string(entry.path().join("device/capabilities/key"))
                .is_ok_and(|bits| has_key(&bits, BTN_LEFT) || has_key(&bits, BTN_TOUCH))
        })
        .filter_map(|entry| {
            OpenOptions::new()
                .read(true)
                .custom_flags(OFlag::O_NONBLOCK.bits())
                .open(format!(
                    "/dev/input/{}",
                    entry.file_name().to_string_lossy()
                ))
                .ok()
        })
        .collect()
}

fn has_key(capabilities: &str, code: u16) -> bool {
    let words: Vec<&str> = capabilities.split_whitespace().rev().collect();
    let word = usize::from(code) / 64;
    words
        .get(word)
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .is_some_and(|bits| bits & (1 << (code % 64)) != 0)
}

#[derive(Default)]
struct Touch {
    down_at: Option<Instant>,
    pressed: bool,
}

fn read_clicks(mut devices: Vec<File>, stop: &AtomicBool) -> Vec<Instant> {
    let mut clicks = Vec::new();
    let mut touches: Vec<Touch> = devices.iter().map(|_| Touch::default()).collect();
    let mut buffer = [0u8; EVENT_SIZE * 64];
    while !stop.load(Ordering::Relaxed) {
        let ready: Vec<bool> = {
            let mut fds: Vec<PollFd> = devices
                .iter()
                .map(|device| PollFd::new(device.as_fd(), PollFlags::POLLIN))
                .collect();
            if poll(&mut fds, PollTimeout::from(POLL_INTERVAL_MS)).is_err() {
                break;
            }
            fds.iter()
                .map(|fd| fd.revents().is_some_and(|r| r.contains(PollFlags::POLLIN)))
                .collect()
        };
        for (index, device) in devices.iter_mut().enumerate().filter(|(i, _)| ready[*i]) {
            loop {
                let read = match device.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => break,
                };
                for event in buffer[..read].as_chunks::<EVENT_SIZE>().0 {
                    let kind = u16::from_ne_bytes([event[16], event[17]]);
                    let code = u16::from_ne_bytes([event[18], event[19]]);
                    let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
                    if let Some(click) =
                        interpret(&mut touches[index], kind, code, value, Instant::now())
                    {
                        clicks.push(click);
                    }
                }
            }
        }
    }
    clicks.sort();
    clicks
}

fn interpret(touch: &mut Touch, kind: u16, code: u16, value: i32, now: Instant) -> Option<Instant> {
    if kind != EV_KEY {
        return None;
    }
    match (code, value) {
        (BTN_LEFT | BTN_RIGHT | BTN_MIDDLE, 1) => {
            touch.pressed = true;
            Some(now)
        }
        (BTN_TOUCH, 1) => {
            *touch = Touch {
                down_at: Some(now),
                pressed: false,
            };
            None
        }
        (BTN_TOUCH, 0) => {
            let tap = touch
                .down_at
                .take()
                .filter(|down| !touch.pressed && now.duration_since(*down) <= TAP_MAX);
            touch.pressed = false;
            tap
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_capability_bitmaps() {
        assert!(has_key("70000 0 0 0 0", BTN_LEFT));
        assert!(has_key("e520 10000 0 0 0 0", BTN_TOUCH));
        assert!(!has_key(
            "2000000000000000 0 40000 0 0 0 0 10100f02902007 f780307cfb10f001 feffffdfffcfffff fffffffffffffffe",
            BTN_LEFT
        ));
        assert!(!has_key("0", BTN_LEFT));
    }

    #[test]
    fn detects_presses_and_taps_once() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut touch = Touch::default();

        assert_eq!(
            interpret(&mut touch, EV_KEY, BTN_LEFT, 1, at(0)),
            Some(at(0))
        );
        assert_eq!(interpret(&mut touch, EV_KEY, BTN_LEFT, 0, at(50)), None);

        assert_eq!(interpret(&mut touch, EV_KEY, BTN_TOUCH, 1, at(1000)), None);
        assert_eq!(
            interpret(&mut touch, EV_KEY, BTN_TOUCH, 0, at(1100)),
            Some(at(1000))
        );

        assert_eq!(interpret(&mut touch, EV_KEY, BTN_TOUCH, 1, at(2000)), None);
        assert_eq!(interpret(&mut touch, EV_KEY, BTN_TOUCH, 0, at(2600)), None);

        assert_eq!(interpret(&mut touch, EV_KEY, BTN_TOUCH, 1, at(3000)), None);
        assert_eq!(
            interpret(&mut touch, EV_KEY, BTN_LEFT, 1, at(3030)),
            Some(at(3030))
        );
        assert_eq!(interpret(&mut touch, EV_KEY, BTN_TOUCH, 0, at(3090)), None);
    }
}
