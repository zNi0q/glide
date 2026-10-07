use std::{
    mem,
    num::NonZeroU32,
    ptr::NonNull,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use egui::{Pos2, Rect, pos2};
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::{
        Event,
        randr::ConnectionExt as _,
        shape::{self, ConnectionExt as _},
        xproto::{
            AtomEnum, ChangeWindowAttributesAux, ClipOrdering, ColormapAlloc, ConfigureWindowAux,
            ConnectionExt as _, CreateWindowAux, Cursor, EventMask, InputFocus, PropMode,
            Rectangle, Screen, StackMode, VisualClass, Visualid, Window, WindowClass,
        },
    },
    wrapper::ConnectionExt as _,
    xcb_ffi::XCBConnection,
};

use super::{Frontend, regions_changed, rounded_rects, toolbar::Toolbar};
use crate::x11::escape_keycode;

const LEFT_PTR: u16 = 68;
const HAND: u16 = 60;
const FLEUR: u16 = 52;
const RAISE_EVERY: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_millis(16);

pub(super) fn run() -> Result<()> {
    let (conn, screen_index) =
        XCBConnection::connect(None).context("cannot connect to the X server")?;
    let screen = conn.setup().roots[screen_index].clone();
    let root = screen.root;
    let visual =
        argb_visual(&screen).context("the X server has no 32-bit visual for transparency")?;
    let area = primary_monitor(&conn, root).unwrap_or(Rectangle {
        x: 0,
        y: 0,
        width: screen.width_in_pixels,
        height: screen.height_in_pixels,
    });

    let colormap = conn.generate_id()?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual)?;
    let window = conn.generate_id()?;
    conn.create_window(
        32,
        window,
        root,
        area.x,
        area.y,
        area.width,
        area.height,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0)
            .border_pixel(0)
            .colormap(colormap)
            .event_mask(
                EventMask::POINTER_MOTION
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::KEY_PRESS
                    | EventMask::LEAVE_WINDOW,
            ),
    )?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"glide",
    )?;
    set_regions(&conn, window, None, &[])?;
    conn.map_window(window)?;
    conn.flush()?;

    let blur = conn
        .intern_atom(false, b"_KDE_NET_WM_BLUR_BEHIND_REGION")?
        .reply()?
        .atom;
    let escape = escape_keycode(&conn)?;
    let cursors = Cursors::new(&conn)?;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let display = NonNull::new(conn.get_raw_xcb_connection()).context("no X connection")?;
    let mut window_handle =
        wgpu::rwh::XcbWindowHandle::new(NonZeroU32::new(window).context("invalid X window")?);
    window_handle.visual_id = NonZeroU32::new(visual);
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(wgpu::rwh::RawDisplayHandle::Xcb(
                wgpu::rwh::XcbDisplayHandle::new(Some(display), screen_index as i32),
            )),
            raw_window_handle: wgpu::rwh::RawWindowHandle::Xcb(window_handle),
        })
    }
    .context("cannot create a GPU surface")?;
    let mut frontend = Frontend::new(&instance, surface)?;
    let size = (u32::from(area.width), u32::from(area.height));
    frontend.configure(size, 1);

    let mut toolbar = Toolbar::new(false);
    toolbar.check_for_update();
    let mut input = Vec::new();
    let mut pointer = Pos2::ZERO;
    let mut applied: Vec<(Rect, f32)> = Vec::new();
    let mut cursor = egui::CursorIcon::Default;
    let mut raised = Instant::now();

    loop {
        while let Some(event) = conn.poll_for_event()? {
            match event {
                Event::MotionNotify(motion) => {
                    pointer = pos2(f32::from(motion.event_x), f32::from(motion.event_y));
                    input.push(egui::Event::PointerMoved(pointer));
                }
                Event::ButtonPress(press) if press.detail == 1 => {
                    conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)?;
                    input.push(primary_button(pointer, true));
                }
                Event::ButtonRelease(release) if release.detail == 1 => {
                    input.push(primary_button(pointer, false));
                }
                Event::LeaveNotify(_) => input.push(egui::Event::PointerGone),
                Event::KeyPress(key) if Some(key.detail) == escape => toolbar.escape(),
                _ => {}
            }
        }

        let output = frontend.run(&mut toolbar, mem::take(&mut input), size, 1.0);
        if toolbar.quit {
            break;
        }
        if regions_changed(&toolbar.regions, &applied) {
            set_regions(&conn, window, Some(blur), &toolbar.regions)?;
            applied = toolbar.regions.clone();
        }
        if output.platform_output.cursor_icon != cursor {
            cursor = output.platform_output.cursor_icon;
            conn.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().cursor(cursors.for_icon(cursor)),
            )?;
        }
        if raised.elapsed() > RAISE_EVERY {
            conn.configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
            raised = Instant::now();
        }
        conn.flush()?;
        if !frontend.paint(output, || {}) {
            frontend.configure(size, 1);
            thread::sleep(RETRY_DELAY);
        }
    }

    conn.destroy_window(window)?;
    conn.flush()?;
    Ok(())
}

fn primary_button(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}

fn argb_visual(screen: &Screen) -> Option<Visualid> {
    screen
        .allowed_depths
        .iter()
        .filter(|depth| depth.depth == 32)
        .flat_map(|depth| &depth.visuals)
        .find(|visual| visual.class == VisualClass::TRUE_COLOR)
        .map(|visual| visual.visual_id)
}

fn primary_monitor(conn: &XCBConnection, root: Window) -> Option<Rectangle> {
    let monitors = conn
        .randr_get_monitors(root, true)
        .ok()?
        .reply()
        .ok()?
        .monitors;
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.primary)
        .or_else(|| monitors.first())?;
    Some(Rectangle {
        x: monitor.x,
        y: monitor.y,
        width: monitor.width,
        height: monitor.height,
    })
}

fn set_regions(
    conn: &XCBConnection,
    window: Window,
    blur: Option<u32>,
    regions: &[(Rect, f32)],
) -> Result<()> {
    let rects: Vec<Rectangle> = rounded_rects(regions)
        .into_iter()
        .map(|[x, y, width, height]| Rectangle {
            x: x as i16,
            y: y as i16,
            width: width.max(0) as u16,
            height: height.max(0) as u16,
        })
        .collect();
    conn.shape_rectangles(
        shape::SO::SET,
        shape::SK::INPUT,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &rects,
    )?;
    if let Some(blur) = blur {
        let area: Vec<u32> = rects
            .iter()
            .flat_map(|r| [r.x as u32, r.y as u32, r.width.into(), r.height.into()])
            .collect();
        conn.change_property32(PropMode::REPLACE, window, blur, AtomEnum::CARDINAL, &area)?;
    }
    Ok(())
}

struct Cursors {
    arrow: Cursor,
    hand: Cursor,
    grabbing: Cursor,
}

impl Cursors {
    fn new(conn: &XCBConnection) -> Result<Self> {
        let font = conn.generate_id()?;
        conn.open_font(font, b"cursor")?;
        let glyph = |shape: u16| -> Result<Cursor> {
            let cursor = conn.generate_id()?;
            conn.create_glyph_cursor(
                cursor,
                font,
                font,
                shape,
                shape + 1,
                0,
                0,
                0,
                u16::MAX,
                u16::MAX,
                u16::MAX,
            )?;
            Ok(cursor)
        };
        let cursors = Self {
            arrow: glyph(LEFT_PTR)?,
            hand: glyph(HAND)?,
            grabbing: glyph(FLEUR)?,
        };
        conn.close_font(font)?;
        Ok(cursors)
    }

    fn for_icon(&self, icon: egui::CursorIcon) -> Cursor {
        match icon {
            egui::CursorIcon::PointingHand => self.hand,
            egui::CursorIcon::Grabbing => self.grabbing,
            _ => self.arrow,
        }
    }
}
