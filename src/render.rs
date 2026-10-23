use irresponsible::error::{Result, invalid};
use std::path::Path;
use vello::{Scene, peniko::Color, util::RenderContext, wgpu};

/// Render the same vector scene used by the window, without screen recording.
pub fn png(scene: &Scene, path: &Path, width: u32, height: u32, background: Color) -> Result<()> {
    let mut context = RenderContext::new();
    let id = pollster::block_on(context.device(None))
        .ok_or_else(|| invalid("No GPU available for rendering"))?;
    let device = &context.devices[id].device;
    let queue = &context.devices[id].queue;
    if width == 0 || height == 0 || width.max(height) > device.limits().max_texture_dimension_2d {
        return Err(invalid("Render dimensions exceed the GPU texture limit"));
    }
    let mut renderer =
        vello::Renderer::new(device, Default::default()).map_err(|e| invalid(e.to_string()))?;
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("IRresponsible preview"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    renderer
        .render_to_texture(
            device,
            queue,
            scene,
            &texture.create_view(&Default::default()),
            &vello::RenderParams {
                base_color: background,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .map_err(|e| invalid(e.to_string()))?;
    let stride = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Preview readback"),
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| invalid(e.to_string()))?;
    rx.recv()
        .map_err(|e| invalid(e.to_string()))?
        .map_err(|e| invalid(e.to_string()))?;
    let mapped = slice.get_mapped_range();
    let pixels: Vec<_> = mapped
        .chunks(stride as usize)
        .flat_map(|row| row[..width as usize * 4].iter().copied())
        .collect();
    drop(mapped);
    buffer.unmap();
    let mut encoder = png::Encoder::new(std::fs::File::create(path)?, width, height);
    encoder.set_color(png::ColorType::Rgba);
    let mut writer = encoder.write_header().map_err(|e| invalid(e.to_string()))?;
    writer
        .write_image_data(&pixels)
        .map_err(|e| invalid(e.to_string()))?;
    Ok(())
}
