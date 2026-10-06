use std::sync::mpsc;

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Params {
    pub camera: [f32; 4],
    pub window_rect: [f32; 4],
    pub cursor: [f32; 4],
    pub sizes: [f32; 4],
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Nv12,
    Rgba,
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    window: wgpu::Texture,
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    window_size: (u32, u32),
    workgroups: (u32, u32),
}

impl Compositor {
    pub fn new(
        window_size: (u32, u32),
        output_size: (u32, u32),
        background_rgba: &[u8],
        output: Output,
    ) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no GPU adapter found")?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .context("cannot open the GPU")?;

        let module = device.create_shader_module(wgpu::include_wgsl!("composite.wgsl"));
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("composite"),
            layout: None,
            module: &module,
            entry_point: Some(match output {
                Output::Nv12 => "main",
                Output::Rgba => "main_rgba",
            }),
            compilation_options: Default::default(),
            cache: None,
        });

        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let window = create_rgba_texture(&device, "window", window_size);
        let background = create_rgba_texture(&device, "background", output_size);
        write_rgba(&queue, &background, background_rgba, output_size);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let (words_per_row, rows) = match output {
            Output::Nv12 => (output_size.0 / 4, output_size.1 * 3 / 2),
            Output::Rgba => (output_size.0, output_size.1),
        };
        let output_bytes = u64::from(words_per_row * rows * 4);
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("output"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: output_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &window.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        &background.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output.as_entire_binding(),
                },
            ],
        });

        Ok(Self {
            device,
            queue,
            pipeline,
            bind_group,
            params,
            window,
            output,
            readback,
            window_size,
            workgroups: (words_per_row.div_ceil(16), rows.div_ceil(16)),
        })
    }

    pub fn render(&self, window_rgba: &[u8], params: &Params, pixels: &mut Vec<u8>) -> Result<()> {
        write_rgba(&self.queue, &self.window, window_rgba, self.window_size);
        self.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(params));

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(self.workgroups.0, self.workgroups.1, 1);
        }
        encoder.copy_buffer_to_buffer(&self.output, 0, &self.readback, 0, None);
        self.queue.submit([encoder.finish()]);

        let slice = self.readback.slice(..);
        let (mapped_tx, mapped_rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = mapped_tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("GPU stopped responding")?;
        mapped_rx
            .recv()
            .context("GPU readback was dropped")?
            .context("cannot read the frame back from the GPU")?;
        let frame = slice
            .get_mapped_range()
            .context("cannot read the frame back from the GPU")?;
        pixels.clear();
        pixels.extend_from_slice(&frame);
        drop(frame);
        self.readback.unmap();
        Ok(())
    }
}

fn create_rgba_texture(device: &wgpu::Device, label: &str, size: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_rgba(queue: &wgpu::Queue, texture: &wgpu::Texture, rgba: &[u8], size: (u32, u32)) {
    queue.write_texture(
        texture.as_image_copy(),
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 4),
            rows_per_image: None,
        },
        extent(size),
    );
}

fn extent((width, height): (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
