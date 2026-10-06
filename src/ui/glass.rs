use std::num::NonZeroU64;

use bytemuck::{Pod, Zeroable};
use egui::{Painter, Rect};
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

const SLOT_STRIDE: u64 = 256;
const SLOTS: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    panel: [f32; 4],
    params: [f32; 4],
}

struct GlassResources {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

pub fn install(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    resources: &mut CallbackResources,
) {
    let module = device.create_shader_module(wgpu::include_wgsl!("glass.wgsl"));
    let uniform_size = NonZeroU64::new(size_of::<Uniform>() as u64);
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("glass"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: uniform_size,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("glass"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("glass"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glass"),
        size: SLOT_STRIDE * u64::from(SLOTS),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("glass"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &uniforms,
                offset: 0,
                size: uniform_size,
            }),
        }],
    });
    resources.insert(GlassResources {
        pipeline,
        uniforms,
        bind_group,
    });
}

pub struct GlassPainter {
    next_slot: u32,
    time: f32,
}

impl GlassPainter {
    pub fn new(time: f32) -> Self {
        Self { next_slot: 0, time }
    }

    pub fn paint(&mut self, painter: &Painter, panel: Rect, radius: f32, opacity: f32) {
        if self.next_slot >= SLOTS {
            return;
        }
        let glass = Glass {
            slot: self.next_slot,
            panel,
            radius,
            time: self.time,
            opacity,
        };
        self.next_slot += 1;
        painter.add(egui_wgpu::Callback::new_paint_callback(
            panel.expand(1.0),
            glass,
        ));
    }
}

struct Glass {
    slot: u32,
    panel: Rect,
    radius: f32,
    time: f32,
    opacity: f32,
}

impl CallbackTrait for Glass {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(glass) = resources.get::<GlassResources>() else {
            return Vec::new();
        };
        let ppp = screen.pixels_per_point;
        let uniform = Uniform {
            panel: [
                self.panel.min.x * ppp,
                self.panel.min.y * ppp,
                self.panel.width() * ppp,
                self.panel.height() * ppp,
            ],
            params: [self.radius * ppp, self.time, self.opacity, ppp],
        };
        queue.write_buffer(
            &glass.uniforms,
            u64::from(self.slot) * SLOT_STRIDE,
            bytemuck::bytes_of(&uniform),
        );
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(glass) = resources.get::<GlassResources>() else {
            return;
        };
        render_pass.set_pipeline(&glass.pipeline);
        render_pass.set_bind_group(0, &glass.bind_group, &[self.slot * SLOT_STRIDE as u32]);
        render_pass.draw(0..3, 0..1);
    }
}
