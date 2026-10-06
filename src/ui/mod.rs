mod glass;
mod jobs;
mod toolbar;

use std::{process::Command, ptr::NonNull, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use egui::{FontData, FontDefinitions, FontFamily, Pos2, Rect, ViewportId, vec2};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};
use wayland_protocols_plasma::blur::client::{
    org_kde_kwin_blur::OrgKdeKwinBlur, org_kde_kwin_blur_manager::OrgKdeKwinBlurManager,
};

use toolbar::Toolbar;

const BTN_LEFT: u32 = 0x110;
const WINDOW_W: u32 = 1040;
const WINDOW_H: u32 = 300;

enum Shell {
    Layer(LayerSurface),
    Window(Window),
}

impl Shell {
    fn surface(&self) -> &wl_surface::WlSurface {
        match self {
            Shell::Layer(layer) => layer.wl_surface(),
            Shell::Window(window) => window.wl_surface(),
        }
    }

    fn commit(&self) {
        self.surface().commit();
    }
}

enum Blur {
    Kde(OrgKdeKwinBlur),
    Standard(ExtBackgroundEffectSurfaceV1),
}

pub fn run() -> Result<()> {
    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor is missing")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm is missing")?;

    let surface = compositor.create_surface(&qh);
    let shell = match LayerShell::bind(&globals, &qh) {
        Ok(layer_shell) => {
            let layer =
                layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("glide"), None);
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_size(0, 0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            Shell::Layer(layer)
        }
        Err(_) => {
            let xdg =
                XdgShell::bind(&globals, &qh).context("the compositor has no window support")?;
            let window = xdg.create_window(surface, WindowDecorations::RequestClient, &qh);
            window.set_title("glide");
            window.set_app_id("glide");
            window.set_min_size(Some((WINDOW_W, WINDOW_H)));
            window.set_max_size(Some((WINDOW_W, WINDOW_H)));
            Shell::Window(window)
        }
    };
    shell.commit();

    let blur = match globals.bind::<OrgKdeKwinBlurManager, _, _>(&qh, 1..=1, ()) {
        Ok(manager) => Some(Blur::Kde(manager.create(shell.surface(), &qh, ()))),
        Err(_) => globals
            .bind::<ExtBackgroundEffectManagerV1, _, _>(&qh, 1..=1, ())
            .ok()
            .map(|manager| Blur::Standard(manager.get_background_effect(shell.surface(), &qh, ()))),
    };

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let display =
        NonNull::new(conn.backend().display_ptr().cast()).context("no Wayland display")?;
    let window =
        NonNull::new(shell.surface().id().as_ptr().cast()).context("no Wayland surface")?;
    let gpu_surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(wgpu::rwh::RawDisplayHandle::Wayland(
                wgpu::rwh::WaylandDisplayHandle::new(display),
            )),
            raw_window_handle: wgpu::rwh::RawWindowHandle::Wayland(
                wgpu::rwh::WaylandWindowHandle::new(window),
            ),
        })
    }
    .context("cannot create a GPU surface")?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&gpu_surface),
        ..Default::default()
    }))
    .context("no GPU adapter can draw to this surface")?;
    let (device, gpu_queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .context("cannot open the GPU")?;

    let caps = gpu_surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(caps.formats[0]);
    let alpha_mode = if caps
        .alpha_modes
        .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
    {
        wgpu::CompositeAlphaMode::PreMultiplied
    } else {
        caps.alpha_modes[0]
    };

    let mut renderer = egui_wgpu::Renderer::new(&device, format, Default::default());
    glass::install(&device, format, &mut renderer.callback_resources);

    let ctx = egui::Context::default();
    install_font(&ctx);

    let mut app = App {
        conn: conn.clone(),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm,
        toolbar: Toolbar::new(matches!(shell, Shell::Window(_))),
        shell,
        blur,
        seat: None,
        press_serial: 0,
        pointer: None,
        keyboard: None,
        cursor: CursorIcon::Default,
        device,
        gpu_queue,
        gpu_surface,
        format,
        alpha_mode,
        renderer,
        ctx,
        input: Vec::new(),
        pointer_pos: Pos2::ZERO,
        size: (0, 0),
        scale: 1,
        configured: false,
        applied_regions: Vec::new(),
        start: Instant::now(),
        exit: false,
    };

    while !app.exit {
        queue.blocking_dispatch(&mut app)?;
    }
    Ok(())
}

fn install_font(ctx: &egui::Context) {
    let path = Command::new("fc-match")
        .args(["-f", "%{file}", "sans-serif:weight=medium"])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    let Some(bytes) = path.and_then(|path| std::fs::read(path).ok()) else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("system".to_owned(), Arc::new(FontData::from_owned(bytes)));
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, "system".to_owned());
    }
    ctx.set_fonts(fonts);
}

struct App {
    conn: Connection,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    shell: Shell,
    blur: Option<Blur>,
    seat: Option<wl_seat::WlSeat>,
    press_serial: u32,
    pointer: Option<ThemedPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    cursor: CursorIcon,
    device: wgpu::Device,
    gpu_queue: wgpu::Queue,
    gpu_surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    renderer: egui_wgpu::Renderer,
    ctx: egui::Context,
    toolbar: Toolbar,
    input: Vec<egui::Event>,
    pointer_pos: Pos2,
    size: (u32, u32),
    scale: i32,
    configured: bool,
    applied_regions: Vec<(Rect, f32)>,
    start: Instant,
    exit: bool,
}

impl App {
    fn configure_gpu_surface(&self) {
        let scale = self.scale.max(1) as u32;
        self.gpu_surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                width: self.size.0 * scale,
                height: self.size.1 * scale,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: self.alpha_mode,
                view_formats: vec![],
                color_space: Default::default(),
            },
        );
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured || self.exit {
            return;
        }
        let scale = self.scale.max(1) as f32;
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(self.size.0 as f32, self.size.1 as f32),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: std::mem::take(&mut self.input),
            focused: true,
            ..Default::default()
        };
        raw.viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(scale);

        let toolbar = &mut self.toolbar;
        let output = self.ctx.run_ui(raw, |ui| toolbar.ui(ui));
        if self.toolbar.quit {
            self.exit = true;
            return;
        }
        if self.toolbar.take_move_request()
            && let (Shell::Window(window), Some(seat)) = (&self.shell, &self.seat)
        {
            window.move_(seat, self.press_serial);
            self.input.push(egui::Event::PointerButton {
                pos: self.pointer_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            });
            self.input.push(egui::Event::PointerGone);
        }
        self.apply_regions(qh);
        self.update_cursor(output.platform_output.cursor_icon);

        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.gpu_queue, *id, delta);
            }
        }

        let frame = match self.gpu_surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            _ => {
                self.configure_gpu_surface();
                self.request_frame(qh);
                self.shell.commit();
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [frame.texture.width(), frame.texture.height()],
            pixels_per_point: output.pixels_per_point,
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let extra = self.renderer.update_buffers(
            &self.device,
            &self.gpu_queue,
            &mut encoder,
            &jobs,
            &screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("toolbar"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &jobs, &screen);
        }
        self.gpu_queue
            .submit(extra.into_iter().chain([encoder.finish()]));
        self.request_frame(qh);
        self.gpu_queue.present(frame);

        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
    }

    fn resize(&mut self, qh: &QueueHandle<Self>, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.size = (width, height);
        self.configure_gpu_surface();
        if !self.configured {
            self.configured = true;
            self.draw(qh);
        }
    }

    fn request_frame(&self, qh: &QueueHandle<Self>) {
        let surface = self.shell.surface();
        surface.frame(qh, FrameCallbackData(surface.clone()));
    }

    fn apply_regions(&mut self, qh: &QueueHandle<Self>) {
        let regions = self.toolbar.regions.clone();
        let unchanged = regions.len() == self.applied_regions.len()
            && regions
                .iter()
                .zip(&self.applied_regions)
                .all(|((a, _), (b, _))| {
                    (a.min - b.min).length() < 0.5 && (a.max - b.max).length() < 0.5
                });
        if unchanged {
            return;
        }
        let _ = qh;
        if let Ok(input) = rounded_region(&self.compositor, &regions) {
            self.shell
                .surface()
                .set_input_region(Some(input.wl_region()));
        }
        if let Some(blur) = &self.blur
            && let Ok(area) = rounded_region(&self.compositor, &regions)
        {
            match blur {
                Blur::Kde(blur) => {
                    blur.set_region(Some(area.wl_region()));
                    blur.commit();
                }
                Blur::Standard(effect) => effect.set_blur_region(Some(area.wl_region())),
            }
        }
        self.applied_regions = regions;
    }

    fn update_cursor(&mut self, icon: egui::CursorIcon) {
        let wanted = match icon {
            egui::CursorIcon::PointingHand => CursorIcon::Pointer,
            egui::CursorIcon::Grabbing => CursorIcon::Grabbing,
            _ => CursorIcon::Default,
        };
        if wanted == self.cursor {
            return;
        }
        self.cursor = wanted;
        if let Some(pointer) = &self.pointer {
            let _ = pointer.set_cursor(&self.conn, wanted);
        }
    }
}

fn rounded_region(compositor: &CompositorState, shapes: &[(Rect, f32)]) -> Result<Region> {
    let region = Region::new(compositor)?;
    for (rect, radius) in shapes {
        let (x, y) = (rect.min.x.round() as i32, rect.min.y.round() as i32);
        let (w, h) = (rect.width().round() as i32, rect.height().round() as i32);
        let r = radius.min(w as f32 / 2.0).min(h as f32 / 2.0).round() as i32;
        region.add(x, y + r, w, h - 2 * r);
        for row in 0..r {
            let dy = r as f32 - row as f32 - 0.5;
            let inset = (r as f32 - (r as f32 * r as f32 - dy * dy).max(0.0).sqrt()).round() as i32;
            region.add(x + inset, y + row, w - 2 * inset, 1);
            region.add(x + inset, y + h - 1 - row, w - 2 * inset, 1);
        }
    }
    Ok(region)
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if surface == self.shell.surface() && new_factor != self.scale {
            self.scale = new_factor;
            surface.set_buffer_scale(new_factor);
            if self.configured {
                self.configure_gpu_surface();
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (w, h) = configure.new_size;
        self.resize(qh, w, h);
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _window: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        let (w, h) = configure.new_size;
        self.resize(
            qh,
            w.map_or(WINDOW_W, |w| w.get()),
            h.map_or(WINDOW_H, |h| h.get()),
        );
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            self.pointer = self
                .seat_state
                .get_pointer_with_theme::<_, ()>(
                    qh,
                    &seat,
                    self.shm.wl_shm(),
                    cursor_surface,
                    ThemeSpec::default(),
                )
                .ok();
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape {
            self.toolbar.escape();
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.shell.surface() {
                continue;
            }
            let pos = Pos2::new(event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.pointer_pos = pos;
                    self.input.push(egui::Event::PointerMoved(pos));
                    if let Some(pointer) = &self.pointer {
                        let _ = pointer.set_cursor(conn, self.cursor);
                    }
                }
                PointerEventKind::Leave { .. } => self.input.push(egui::Event::PointerGone),
                PointerEventKind::Motion { .. } => {
                    self.pointer_pos = pos;
                    self.input.push(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Press { button, .. }
                | PointerEventKind::Release { button, .. }
                    if button == BTN_LEFT =>
                {
                    if let PointerEventKind::Press { serial, .. } = event.kind {
                        self.press_serial = serial;
                    }
                    self.input.push(egui::Event::PointerButton {
                        pos: self.pointer_pos,
                        button: egui::PointerButton::Primary,
                        pressed: matches!(event.kind, PointerEventKind::Press { .. }),
                        modifiers: Default::default(),
                    });
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

impl Dispatch<OrgKdeKwinBlurManager, ()> for App {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinBlurManager,
        _: <OrgKdeKwinBlurManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        _: <ExtBackgroundEffectManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ExtBackgroundEffectSurfaceV1,
        _: <ExtBackgroundEffectSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<OrgKdeKwinBlur, ()> for App {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinBlur,
        _: <OrgKdeKwinBlur as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_registry!(App);
delegate_dispatch2!(App);
