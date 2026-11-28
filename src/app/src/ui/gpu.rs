//! Shared wgpu GPU context and multi-window egui rendering wrapper (Main / Overlay / Pin / Recorder).

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use winit::event::WindowEvent;
use winit::window::Window;

pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    pub fn new(initial_window: &Arc<Window>) -> Result<Self, String> {
        // Prefer DX12 on Windows to avoid probing Vulkan/OpenGL drivers unnecessarily.
        let mut selected: Option<(wgpu::Instance, wgpu::Adapter)> = None;
        for backends in [
            wgpu::Backends::DX12,
            wgpu::Backends::VULKAN | wgpu::Backends::GL,
        ] {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            });
            let Ok(probe_surface) = instance.create_surface(initial_window.clone()) else {
                continue;
            };
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&probe_surface),
                    force_fallback_adapter: false,
                }))
                .or_else(|| {
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: Some(&probe_surface),
                        force_fallback_adapter: true,
                    }))
                });
            drop(probe_surface);
            if let Some(adapter) = adapter {
                selected = Some((instance, adapter));
                break;
            }
        }
        let (instance, adapter) =
            selected.ok_or_else(|| "No compatible graphics adapter found".to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("snapdock-gpu"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            None,
        ))
        .map_err(|e| format!("Failed to initialize graphics device: {}", e))?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }
}

enum WindowTarget {
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Layered {
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        staging: wgpu::Buffer,
        padded_bytes_per_row: u32,
        dib: crate::native::LayeredDib,
        width: u32,
        height: u32,
        opacity: u8,
        presented_once: bool,
    },
}

fn create_layered_resources(
    gpu: &GpuContext,
    width: u32,
    height: u32,
) -> Result<
    (
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::Buffer,
        u32,
        crate::native::LayeredDib,
    ),
    String,
> {
    let width = width.max(1);
    let height = height.max(1);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapdock-layered-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let unpadded_bytes_per_row = width * 4;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("snapdock-layered-staging"),
        size: (padded_bytes_per_row as u64) * (height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let dib = crate::native::LayeredDib::new(width, height)?;
    Ok((texture, view, staging, padded_bytes_per_row, dib))
}

pub struct EguiWindow {
    pub window: Arc<Window>,
    target: WindowTarget,
    pub egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    clear_color: wgpu::Color,
}

impl EguiWindow {
    pub fn new(
        gpu: &GpuContext,
        window: Arc<Window>,
        clear_color: wgpu::Color,
    ) -> Result<Self, String> {
        let surface = gpu
            .instance
            .create_surface(window.clone())
            .map_err(|e| format!("Failed to create window surface: {}", e))?;
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        let size = window.inner_size();
        let alpha_mode = caps
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes
                .first()
                .copied()
                .unwrap_or(wgpu::PresentMode::Fifo)
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&gpu.device, &config);

        let egui_ctx = egui::Context::default();
        super::configure_egui(&egui_ctx);
        let viewport_id = egui_ctx.viewport_id();
        let max_texture_side = gpu.device.limits().max_texture_dimension_2d as usize;
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            Some(max_texture_side),
        );
        let renderer = egui_wgpu::Renderer::new(&gpu.device, format, None, 1, false);
        Ok(Self {
            window,
            target: WindowTarget::Surface { surface, config },
            egui_ctx,
            egui_state,
            renderer,
            clear_color,
        })
    }

    /// Create an offscreen-rendered per-pixel alpha layered window presented via Win32
    /// `UpdateLayeredWindow` (`AC_SRC_ALPHA` + `SourceConstantAlpha`).
    pub fn new_layered(
        gpu: &GpuContext,
        window: Arc<Window>,
        opacity: u8,
    ) -> Result<Self, String> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let (texture, view, staging, padded_bytes_per_row, dib) =
            create_layered_resources(gpu, width, height)?;

        let egui_ctx = egui::Context::default();
        super::configure_egui(&egui_ctx);
        let viewport_id = egui_ctx.viewport_id();
        let max_texture_side = gpu.device.limits().max_texture_dimension_2d as usize;
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            Some(max_texture_side),
        );
        let renderer =
            egui_wgpu::Renderer::new(&gpu.device, wgpu::TextureFormat::Bgra8Unorm, None, 1, false);
        Ok(Self {
            window,
            target: WindowTarget::Layered {
                texture,
                view,
                staging,
                padded_bytes_per_row,
                dib,
                width,
                height,
                opacity,
                presented_once: false,
            },
            egui_ctx,
            egui_state,
            renderer,
            clear_color: wgpu::Color::TRANSPARENT,
        })
    }

    pub fn set_clear_color(&mut self, color: wgpu::Color) {
        self.clear_color = color;
    }

    /// Update global window opacity on a layered window immediately without re-rendering GPU content.
    pub fn set_layered_opacity(&mut self, opacity: u8) {
        let hwnd = self.hwnd();
        if let WindowTarget::Layered {
            dib,
            opacity: cur,
            presented_once,
            ..
        } = &mut self.target
        {
            *cur = opacity;
            if *presented_once {
                if let Some(h) = hwnd {
                    let _ = dib.present(h, opacity);
                }
            }
        }
    }

    pub fn hwnd(&self) -> Option<isize> {
        let handle = self.window.window_handle().ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }

    pub fn resize(&mut self, gpu: &GpuContext, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        match &mut self.target {
            WindowTarget::Surface { surface, config } => {
                if config.width == width && config.height == height {
                    return;
                }
                config.width = width;
                config.height = height;
                surface.configure(&gpu.device, config);
            }
            WindowTarget::Layered {
                texture,
                view,
                staging,
                padded_bytes_per_row,
                dib,
                width: cur_w,
                height: cur_h,
                presented_once,
                ..
            } => {
                if *cur_w == width && *cur_h == height {
                    return;
                }
                if let Ok((new_tex, new_view, new_staging, new_padded, new_dib)) =
                    create_layered_resources(gpu, width, height)
                {
                    *texture = new_tex;
                    *view = new_view;
                    *staging = new_staging;
                    *padded_bytes_per_row = new_padded;
                    *dib = new_dib;
                    *cur_w = width;
                    *cur_h = height;
                    *presented_once = false;
                }
            }
        }
    }

    pub fn handle_event(&mut self, event: &WindowEvent) -> egui_winit::EventResponse {
        self.egui_state.on_window_event(self.window.as_ref(), event)
    }

    /// Parse the configured CJK font and upload the initial font atlas texture to the GPU once,
    /// without presenting a visible frame. Eliminates first-frame font parsing stalls on hotkey press.
    pub fn warmup_fonts(&mut self, gpu: &GpuContext) {
        let raw_input = self.egui_state.take_egui_input(self.window.as_ref());
        let full_output = self.egui_ctx.run(raw_input, |ctx| {
            ctx.fonts(|f| {
                let _ = f.layout_no_wrap(
                    "SnapDock 0123456789 截图".to_owned(),
                    egui::FontId::default(),
                    egui::Color32::WHITE,
                );
            });
        });
        for (id, delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(&gpu.device, &gpu.queue, *id, delta);
        }
        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }
    }

    /// Cleanup before returning a hidden overlay window to the pool:
    /// clears transient input/focus/popups, frees dropped screenshot textures from GPU, and shrinks the swapchain.
    pub fn park_for_pool(&mut self, gpu: &GpuContext) {
        let _ = self
            .egui_state
            .on_window_event(self.window.as_ref(), &WindowEvent::Focused(false));
        let _ = self.egui_state.take_egui_input(self.window.as_ref());
        self.egui_ctx.memory_mut(|m| {
            m.close_popup();
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        let delta = self.egui_ctx.tex_manager().write().take_delta();
        for (id, image_delta) in &delta.set {
            self.renderer
                .update_texture(&gpu.device, &gpu.queue, *id, image_delta);
        }
        for id in &delta.free {
            self.renderer.free_texture(id);
        }
        self.resize(gpu, 1, 1);
    }

    /// Reset transient input and sync DPI scale before reusing a pooled overlay window.
    pub fn prepare_for_reuse(&mut self) {
        let _ = self.egui_state.take_egui_input(self.window.as_ref());
        let ppp = self.window.scale_factor() as f32;
        if ppp > 0.0 {
            self.egui_ctx.set_pixels_per_point(ppp);
        }
    }

    pub fn render(
        &mut self,
        gpu: &GpuContext,
        mut draw_ui: impl FnMut(&egui::Context),
    ) -> Option<std::time::Duration> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        let (target_w, target_h) = match &self.target {
            WindowTarget::Surface { config, .. } => (config.width, config.height),
            WindowTarget::Layered { width, height, .. } => (*width, *height),
        };
        if size.width != target_w || size.height != target_h {
            self.resize(gpu, size.width, size.height);
        }

        let raw_input = self.egui_state.take_egui_input(self.window.as_ref());
        let mut full_output = self.egui_ctx.run(raw_input, |ctx| {
            draw_ui(ctx);
        });
        super::apply_font_effects_to_shapes(&self.egui_ctx, &mut full_output.shapes);
        self.egui_state
            .handle_platform_output(self.window.as_ref(), full_output.platform_output);

        let tris = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        for (id, delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(&gpu.device, &gpu.queue, *id, delta);
        }

        let hwnd = self.hwnd();
        match &mut self.target {
            WindowTarget::Surface { surface, config } => {
                let frame = match surface.get_current_texture() {
                    Ok(f) => f,
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        surface.configure(&gpu.device, config);
                        self.window.request_redraw();
                        return None;
                    }
                    Err(_) => return None,
                };
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let screen_desc = egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [config.width, config.height],
                    pixels_per_point: full_output.pixels_per_point,
                };
                let mut encoder = gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("snapdock-egui-encoder"),
                    });
                self.renderer.update_buffers(
                    &gpu.device,
                    &gpu.queue,
                    &mut encoder,
                    &tris,
                    &screen_desc,
                );
                {
                    let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("snapdock-egui-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(self.clear_color),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                    });
                    self.renderer
                        .render(&mut rpass.forget_lifetime(), &tris, &screen_desc);
                }
                gpu.queue.submit(Some(encoder.finish()));
                frame.present();
            }
            WindowTarget::Layered {
                texture,
                view,
                staging,
                padded_bytes_per_row,
                dib,
                width,
                height,
                opacity,
                presented_once,
            } => {
                let screen_desc = egui_wgpu::ScreenDescriptor {
                    size_in_pixels: [*width, *height],
                    pixels_per_point: full_output.pixels_per_point,
                };
                let mut encoder = gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("snapdock-layered-encoder"),
                    });
                self.renderer.update_buffers(
                    &gpu.device,
                    &gpu.queue,
                    &mut encoder,
                    &tris,
                    &screen_desc,
                );
                {
                    let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("snapdock-layered-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(self.clear_color),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                    });
                    self.renderer
                        .render(&mut rpass.forget_lifetime(), &tris, &screen_desc);
                }
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: staging,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(*padded_bytes_per_row),
                            rows_per_image: Some(*height),
                        },
                    },
                    wgpu::Extent3d {
                        width: *width,
                        height: *height,
                        depth_or_array_layers: 1,
                    },
                );
                gpu.queue.submit(Some(encoder.finish()));

                let slice = staging.slice(..);
                let (tx, rx) = std::sync::mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |res| {
                    let _ = tx.send(res);
                });
                gpu.device.poll(wgpu::Maintain::Wait);
                if rx.recv().ok().and_then(|r| r.ok()).is_some() {
                    {
                        let mapped = slice.get_mapped_range();
                        dib.copy_from_padded_bgra(&mapped, *padded_bytes_per_row as usize);
                    }
                    staging.unmap();
                    if let Some(h) = hwnd {
                        if dib.present(h, *opacity).is_ok() {
                            *presented_once = true;
                        }
                    }
                }
            }
        }

        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        let min_delay = full_output
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min();
        if let Some(delay) = min_delay {
            if delay.is_zero() {
                self.window.request_redraw();
                return None;
            }
            if delay < std::time::Duration::from_secs(3600) {
                return Some(delay);
            }
        }
        None
    }
}
