use std::{
    collections::VecDeque,
    env,
    sync::{Mutex, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use x11rb::{
    CURRENT_TIME, NONE,
    connection::Connection,
    protocol::{
        Event,
        composite::{ConnectionExt as _, Redirect},
        xinput::{ConnectionExt as _, EventMask, XIEventMask},
        xproto::{
            AtomEnum, ConnectionExt as _, EventMask as CoreEventMask, GrabMode, GrabStatus,
            ImageFormat, Window,
        },
    },
    rust_connection::RustConnection,
};

use crate::{
    FPS,
    clicks::ClickListener,
    record::{Cancelled, Frame, Image, Latest, Source, StopSignal},
};

const CROSSHAIR: u16 = 34;
const ESCAPE: u32 = 0xff1b;
const ALL_MASTER_DEVICES: u16 = 1;
const CLICK_POLL: Duration = Duration::from_millis(10);

pub fn is_session() -> bool {
    env::var_os("WAYLAND_DISPLAY").is_none_or(|display| display.is_empty())
        && env::var_os("DISPLAY").is_some_and(|display| !display.is_empty())
}

pub struct Target {
    conn: RustConnection,
    root: Window,
    window: Window,
}

impl Target {
    pub fn choose(source: Source) -> Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("cannot connect to the X server")?;
        let root = conn.setup().roots[screen].root;
        let window = match source {
            Source::Screen => root,
            Source::Window => pick_window(&conn, root)?,
        };
        if window != root {
            conn.composite_query_version(0, 4)?
                .reply()
                .context("the X server has no Composite extension")?;
            conn.composite_redirect_window(window, Redirect::AUTOMATIC)?
                .check()
                .context("cannot capture that window")?;
        }
        Ok(Self { conn, root, window })
    }

    pub fn stream(&self, latest: &Mutex<Latest>, stop: &StopSignal) -> Result<()> {
        let period = Duration::from_secs(1) / FPS;
        let mut next = Instant::now();
        while !stop.is_stopped() {
            let Some(frame) = self.grab()? else { break };
            let cursor = self.cursor(frame.width, frame.height)?;
            {
                let mut latest = latest.lock().expect("capture state poisoned");
                latest.frame = Some(frame);
                latest.cursor = cursor;
            }
            next += period;
            match next.checked_duration_since(Instant::now()) {
                Some(wait) => thread::sleep(wait),
                None => next = Instant::now(),
            }
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Image> {
        self.grab()?
            .map(Frame::into_image)
            .context("the window closed before it could be captured")
    }

    fn grab(&self) -> Result<Option<Frame>> {
        let Ok(geometry) = self.conn.get_geometry(self.window)?.reply() else {
            return Ok(None);
        };
        let drawable = if self.window == self.root {
            self.root
        } else {
            let pixmap = self.conn.generate_id()?;
            if self
                .conn
                .composite_name_window_pixmap(self.window, pixmap)?
                .check()
                .is_err()
            {
                return Ok(None);
            }
            pixmap
        };
        let image = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                drawable,
                0,
                0,
                geometry.width,
                geometry.height,
                !0,
            )?
            .reply();
        if drawable != self.root {
            self.conn.free_pixmap(drawable)?;
        }
        let Ok(image) = image else { return Ok(None) };

        let (width, height) = (usize::from(geometry.width), usize::from(geometry.height));
        if image.data.len() != width * height * 4 {
            bail!("unsupported X11 pixel format ({} bits deep)", image.depth);
        }
        let pix_fmt = if image.depth == 32 { "bgra" } else { "bgr0" };
        Ok(Some(Frame {
            width,
            height,
            pix_fmt,
            data: image.data,
        }))
    }

    fn cursor(&self, width: usize, height: usize) -> Result<Option<(f32, f32)>> {
        let pointer = self.conn.query_pointer(self.root)?.reply()?;
        let origin = self
            .conn
            .translate_coordinates(self.window, self.root, 0, 0)?
            .reply()?;
        let x = f32::from(pointer.root_x) - f32::from(origin.dst_x);
        let y = f32::from(pointer.root_y) - f32::from(origin.dst_y);
        let inside = (0.0..width as f32).contains(&x) && (0.0..height as f32).contains(&y);
        Ok(inside.then_some((x, y)))
    }
}

pub fn listen_clicks() -> Option<ClickListener> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen].root;
    conn.xinput_xi_query_version(2, 0).ok()?.reply().ok()?;
    conn.xinput_xi_select_events(
        root,
        &[EventMask {
            deviceid: ALL_MASTER_DEVICES,
            mask: vec![XIEventMask::RAW_BUTTON_PRESS],
        }],
    )
    .ok()?
    .check()
    .ok()?;
    Some(ClickListener::spawn(move |stop| {
        let mut clicks = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            while let Ok(Some(event)) = conn.poll_for_event() {
                if let Event::XinputRawButtonPress(press) = event
                    && (1..=3).contains(&press.detail)
                {
                    clicks.push(Instant::now());
                }
            }
            thread::sleep(CLICK_POLL);
        }
        clicks
    }))
}

fn pick_window(conn: &RustConnection, root: Window) -> Result<Window> {
    let font = conn.generate_id()?;
    conn.open_font(font, b"cursor")?;
    let cursor = conn.generate_id()?;
    conn.create_glyph_cursor(
        cursor,
        font,
        font,
        CROSSHAIR,
        CROSSHAIR + 1,
        0,
        0,
        0,
        u16::MAX,
        u16::MAX,
        u16::MAX,
    )?;
    let grab = conn
        .grab_pointer(
            false,
            root,
            CoreEventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            cursor,
            CURRENT_TIME,
        )?
        .reply()?;
    if grab.status != GrabStatus::SUCCESS {
        bail!("another program is holding the pointer, try again");
    }
    conn.grab_keyboard(false, root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)?
        .reply()?;
    let escape = escape_keycode(conn)?;

    let picked = loop {
        match conn.wait_for_event()? {
            Event::ButtonPress(press) => {
                break Ok(if press.child == NONE {
                    root
                } else {
                    press.child
                });
            }
            Event::KeyPress(key) if Some(key.detail) == escape => {
                break Err(anyhow::Error::new(Cancelled));
            }
            _ => {}
        }
    };
    conn.ungrab_pointer(CURRENT_TIME)?;
    conn.ungrab_keyboard(CURRENT_TIME)?;
    conn.free_cursor(cursor)?;
    conn.close_font(font)?;
    conn.flush()?;
    client_window(conn, picked?)
}

fn client_window(conn: &RustConnection, window: Window) -> Result<Window> {
    let wm_state = conn.intern_atom(false, b"WM_STATE")?.reply()?.atom;
    let mut queue = VecDeque::from([window]);
    while let Some(candidate) = queue.pop_front() {
        let property = conn
            .get_property(false, candidate, wm_state, AtomEnum::ANY, 0, 0)?
            .reply()?;
        if property.type_ != NONE {
            return Ok(candidate);
        }
        queue.extend(conn.query_tree(candidate)?.reply()?.children);
    }
    Ok(window)
}

pub(crate) fn escape_keycode(conn: &impl Connection) -> Result<Option<u8>> {
    let setup = conn.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode - first + 1;
    let mapping = conn.get_keyboard_mapping(first, count)?.reply()?;
    let per_key = usize::from(mapping.keysyms_per_keycode).max(1);
    Ok(mapping
        .keysyms
        .chunks(per_key)
        .position(|syms| syms.contains(&ESCAPE))
        .and_then(|index| u8::try_from(index).ok())
        .map(|index| first + index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn find_window(conn: &RustConnection, root: Window, name: &[u8]) -> Option<Window> {
        let mut queue = VecDeque::from([root]);
        while let Some(window) = queue.pop_front() {
            let title = conn
                .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 64)
                .ok()?
                .reply()
                .ok()?;
            if title.value.windows(name.len()).any(|w| w == name) {
                return Some(window);
            }
            queue.extend(conn.query_tree(window).ok()?.reply().ok()?.children);
        }
        None
    }

    #[test]
    #[ignore = "needs an X server and glxgears"]
    fn captures_an_x11_window() {
        let mut gears = Command::new("glxgears")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (conn, screen) = x11rb::connect(None).unwrap();
        let root = conn.setup().roots[screen].root;
        let window = (0..50)
            .find_map(|_| {
                thread::sleep(Duration::from_millis(100));
                find_window(&conn, root, b"glxgears")
            })
            .expect("glxgears window appears");
        conn.composite_redirect_window(window, Redirect::AUTOMATIC)
            .unwrap()
            .check()
            .unwrap();
        let target = Target { conn, root, window };

        thread::sleep(Duration::from_secs(1));
        let frames: Vec<Frame> = (0..10)
            .map(|_| {
                thread::sleep(Duration::from_millis(100));
                target.grab().unwrap().expect("frame")
            })
            .collect();
        let first = &frames[0];
        assert!(first.width > 100 && first.height > 100);
        assert!(
            frames
                .iter()
                .any(|f| f.data.chunks(4).any(|px| px[..3] != [0, 0, 0])),
            "frames have content"
        );
        gears.kill().unwrap();
        gears.wait().unwrap();
    }
}
