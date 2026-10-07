mod glass;
mod jobs;
mod toolbar;
mod wayland;
mod xorg;

use std::{process::Command, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use egui::{FontData, FontDefinitions, FontFamily, Pos2, Rect, ViewportId, vec2};

use toolbar::Toolbar;

pub fn run() -> Result<()> {
    if crate::x11::is_session() {
        xorg::run()
    } else {
        wayland::run()
    }
}

struct Frontend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    renderer: egui_wgpu::Renderer,
    ctx: egui::Context,
    start: Instant,
}

impl Frontend {
    fn new(instance: &wgpu::Instance, surface: wgpu::Surface<'static>) -> Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no GPU adapter can draw to this surface")?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .context("cannot open the GPU")?;

        let caps = surface.get_capabilities(&adapter);
        let format = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]
        .into_iter()
        .find(|format| caps.formats.contains(format))
        .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
        .unwrap_or(caps.formats[0]);
        let alpha_mode = [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
        .into_iter()
        .find(|mode| caps.alpha_modes.contains(mode))
        .unwrap_or(caps.alpha_modes[0]);

        let mut renderer = egui_wgpu::Renderer::new(&device, format, Default::default());
        glass::install(&device, format, &mut renderer.callback_resources);
        let ctx = egui::Context::default();
        install_font(&ctx);

        Ok(Self {
            device,
            queue,
            surface,
            format,
            alpha_mode,
            renderer,
            ctx,
            start: Instant::now(),
        })
    }

    fn configure(&self, (width, height): (u32, u32), scale: u32) {
        self.surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                width: width * scale,
                height: height * scale,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: self.alpha_mode,
                view_formats: vec![],
                color_space: Default::default(),
            },
        );
    }

    fn run(
        &mut self,
        toolbar: &mut Toolbar,
        events: Vec<egui::Event>,
        (width, height): (u32, u32),
        scale: f32,
    ) -> egui::FullOutput {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(width as f32, height as f32),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            events,
            focused: true,
            ..Default::default()
        };
        raw.viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(scale);
        self.ctx.run_ui(raw, |ui| toolbar.ui(ui))
    }

    fn paint(&mut self, output: egui::FullOutput, before_present: impl FnOnce()) -> bool {
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            _ => return false,
        };
        let view = frame.texture.create_view(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [frame.texture.width(), frame.texture.height()],
            pixels_per_point: output.pixels_per_point,
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let extra =
            self.renderer
                .update_buffers(&self.device, &self.queue, &mut encoder, &jobs, &screen);
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
        self.queue
            .submit(extra.into_iter().chain([encoder.finish()]));
        before_present();
        self.queue.present(frame);

        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        true
    }
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

fn rounded_rects(shapes: &[(Rect, f32)]) -> Vec<[i32; 4]> {
    let mut rects = Vec::new();
    for (rect, radius) in shapes {
        let (x, y) = (rect.min.x.round() as i32, rect.min.y.round() as i32);
        let (w, h) = (rect.width().round() as i32, rect.height().round() as i32);
        let r = radius.min(w as f32 / 2.0).min(h as f32 / 2.0).round() as i32;
        rects.push([x, y + r, w, h - 2 * r]);
        for row in 0..r {
            let dy = r as f32 - row as f32 - 0.5;
            let inset = (r as f32 - (r as f32 * r as f32 - dy * dy).max(0.0).sqrt()).round() as i32;
            rects.push([x + inset, y + row, w - 2 * inset, 1]);
            rects.push([x + inset, y + h - 1 - row, w - 2 * inset, 1]);
        }
    }
    rects
}

fn regions_changed(current: &[(Rect, f32)], applied: &[(Rect, f32)]) -> bool {
    current.len() != applied.len()
        || current.iter().zip(applied).any(|((a, _), (b, _))| {
            (a.min - b.min).length() >= 0.5 || (a.max - b.max).length() >= 0.5
        })
}
