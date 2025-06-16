use crate::terminal::TerminalState;
use glam::Vec2;
use screen_grid::{CellFlags, Rgb};
use std::sync::Arc;
use wgpu::{
    util::{DeviceExt, StagingBelt},
    *,
};
use wgpu_glyph::{
    GlyphBrush, GlyphBrushBuilder, Section, Text,
    ab_glyph::{self, Font, FontArc, ScaleFont},
};
use winit::window::{Window, WindowId};

/// Compile-time embedded font
const FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/fonts/HackNerdFontMono-Regular.ttf"
));

const CELL_H: f32 = 16.0;
const STAGING_CHUNK: usize = 1 << 16;

pub struct Renderer {
    pub window: Arc<Window>,
    gpu: GpuState,
    text: TextRenderer,
    bg: BgRenderer,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct BgVertex {
    position: [f32; 2],
}

impl BgVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x2];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// A single quad has 2 triangles (6 verts)
const BG_VERTICES: &[BgVertex] = &[
    BgVertex {
        position: [0.0, 0.0],
    },
    BgVertex {
        position: [1.0, 0.0],
    },
    BgVertex {
        position: [0.0, 1.0],
    },
    BgVertex {
        position: [1.0, 0.0],
    },
    BgVertex {
        position: [1.0, 1.0],
    },
    BgVertex {
        position: [0.0, 1.0],
    },
];

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct BgInstance {
    /// top-left corner of the cell, in px
    position: [f32; 2],
    /// background color
    color: [f32; 4],
}

impl BgInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![1 => Float32x2, 2 => Float32x4];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[derive(Debug)]
struct BgRenderer {
    pipeline: RenderPipeline,
    vertex_buffer: Buffer,
    instances: Vec<BgInstance>,
    instance_buffer: Buffer,
    globals_bind_group: BindGroup,
    globals_buffer: Buffer,
}

#[derive(Debug)]
struct GpuState {
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,
}

#[derive(Debug)]
struct TextRenderer {
    brush: GlyphBrush<()>,
    staging_belt: StagingBelt,
    cell: Vec2,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        let gpu = GpuState::new(window.as_ref()).await;
        let text = TextRenderer::new(&gpu.device, gpu.config.format);

        let bg = BgRenderer::new(
            &gpu.device,
            gpu.config.format,
            (gpu.config.width, gpu.config.height),
            (text.cell.x, text.cell.y),
        );

        Self {
            window,
            gpu,
            text,
            bg,
        }
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn cell_size(&self) -> (u32, u32) {
        (self.text.cell.x as u32, self.text.cell.y as u32)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        if (w, h) == (self.gpu.config.width, self.gpu.config.height) {
            return;
        }

        self.gpu.config.width = w;
        self.gpu.config.height = h;
        self.gpu
            .surface
            .configure(&self.gpu.device, &self.gpu.config);

        let (cell_w, cell_h) = self.cell_size();
        self.gpu.queue.write_buffer(
            &self.bg.globals_buffer,
            0,
            bytemuck::cast_slice(&[w as f32, h as f32, cell_w as f32, cell_h as f32]),
        );
    }

    pub fn render(&mut self, term: &mut TerminalState) {
        self.text.staging_belt.recall();

        let frame = match self.gpu.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.resize(self.gpu.config.width, self.gpu.config.height);
                return;
            }
            Err(SurfaceError::OutOfMemory) => panic!("GPU out of memory"),
            Err(e) => {
                log::error!("surface: {e:?}");
                return;
            }
        };

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("terminal-encoder"),
            });

        self.queue_bg_rects(term);
        self.queue_cursor(term);
        self.gpu.queue.write_buffer(
            &self.bg.instance_buffer,
            0,
            bytemuck::cast_slice(&self.bg.instances),
        );

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bg/clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Draw bg rects
            rpass.set_pipeline(&self.bg.pipeline);
            rpass.set_bind_group(0, &self.bg.globals_bind_group, &[]);
            rpass.set_vertex_buffer(0, self.bg.vertex_buffer.slice(..));
            rpass.set_vertex_buffer(1, self.bg.instance_buffer.slice(..));
            rpass.draw(
                0..BG_VERTICES.len() as u32,
                0..self.bg.instances.len() as u32,
            );
        }

        self.queue_glyphs(term);

        self.text
            .brush
            .draw_queued(
                &self.gpu.device,
                &mut self.text.staging_belt,
                &mut encoder,
                &view,
                self.gpu.config.width,
                self.gpu.config.height,
            )
            .expect("glyph_brush::draw_queued");

        self.text.staging_belt.finish();
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    fn queue_bg_rects(&mut self, term: &mut TerminalState) {
        self.bg.instances.clear();
        let cell_size = Vec2::new(self.text.cell.x, self.text.cell.y);

        for y in 0..term.grid.rows {
            if let Some(row) = term.grid.get_display_row(y, term.scroll_offset) {
                for (x, cell_data) in row.iter().enumerate() {
                    // Default background is black, so we don't need to draw it
                    if cell_data.bg != Rgb(0, 0, 0) {
                        let color = [
                            cell_data.bg.0 as f32 / 255.0,
                            cell_data.bg.1 as f32 / 255.0,
                            cell_data.bg.2 as f32 / 255.0,
                            1.0,
                        ];
                        self.bg.instances.push(BgInstance {
                            position: [x as f32 * cell_size.x, y as f32 * cell_size.y],
                            color,
                        });
                    }
                }
            }
        }
    }

    fn queue_glyphs(&mut self, term: &mut TerminalState) {
        let TextRenderer {
            brush,
            cell: cell_size,
            ..
        } = &mut self.text;

        let cursor_pos = if term.scroll_offset == 0 {
            Some((term.grid.cur_x, term.grid.cur_y))
        } else {
            None
        };

        let process_run = |text: &mut String,
                           attrs: Option<(Rgb, CellFlags)>,
                           _start_x: usize,
                           px: f32,
                           py: f32,
                           brush: &mut GlyphBrush<()>| {
            if text.is_empty() {
                return;
            }

            if let Some((fg_color, flags)) = attrs {
                let mut rgba = [
                    fg_color.0 as f32 / 255.0,
                    fg_color.1 as f32 / 255.0,
                    fg_color.2 as f32 / 255.0,
                    1.0,
                ];

                if flags.contains(CellFlags::FAINT) {
                    for chan in &mut rgba[0..3] {
                        *chan *= 0.5;
                    }
                }

                brush.queue(Section {
                    screen_position: (px, py),
                    text: vec![Text::new(text).with_color(rgba).with_scale(CELL_H)],
                    ..Section::default()
                });
            }
            text.clear();
        };

        for y in 0..term.grid.rows {
            if let Some(row) = term.grid.get_display_row(y, term.scroll_offset) {
                let mut current_run_text = String::new();
                let mut current_run_attrs: Option<(Rgb, CellFlags)> = None;
                let mut current_run_start_x: usize = 0;

                for (x, cell_data) in row.iter().enumerate() {
                    if cursor_pos.is_some() && cursor_pos.unwrap() == (x, y) {
                        let px = current_run_start_x as f32 * cell_size.x;
                        let py = y as f32 * cell_size.y;
                        process_run(
                            &mut current_run_text,
                            current_run_attrs,
                            current_run_start_x,
                            px,
                            py,
                            brush,
                        );

                        current_run_start_x = x + 1;
                        current_run_attrs = None;
                        continue;
                    }

                    let attrs = (cell_data.fg, cell_data.flags);
                    let is_glyph_with_same_style =
                        cell_data.ch != ' ' && Some(attrs) == current_run_attrs;

                    if is_glyph_with_same_style {
                        // This glyph can extend the current run
                        current_run_text.push(cell_data.ch);
                    } else {
                        // Process the old run, then start a new one
                        let px = current_run_start_x as f32 * cell_size.x;
                        let py = y as f32 * cell_size.y;
                        process_run(
                            &mut current_run_text,
                            current_run_attrs,
                            current_run_start_x,
                            px,
                            py,
                            brush,
                        );

                        if cell_data.ch != ' ' {
                            current_run_start_x = x;
                            current_run_attrs = Some(attrs);
                            current_run_text.push(cell_data.ch);
                        } else {
                            current_run_attrs = None;
                        }
                    }
                }

                // Process the final run of the line
                let px = current_run_start_x as f32 * cell_size.x;
                let py = y as f32 * cell_size.y;
                process_run(
                    &mut current_run_text,
                    current_run_attrs,
                    current_run_start_x,
                    px,
                    py,
                    brush,
                );
            }
        }
    }

    fn queue_cursor(&mut self, term: &TerminalState) {
        // Don't render cursor if scrolled back
        if term.scroll_offset != 0 {
            return;
        }

        let (cx, cy) = (term.grid.cur_x, term.grid.cur_y);

        let cell_under_cursor = term
            .grid
            .visible_row(cy)
            .and_then(|r| r.get(cx))
            .cloned()
            .unwrap_or_default();

        let cursor_bg_color = cell_under_cursor.fg;
        let cursor_bg_rgba = [
            cursor_bg_color.0 as f32 / 255.0,
            cursor_bg_color.1 as f32 / 255.0,
            cursor_bg_color.2 as f32 / 255.0,
            1.0,
        ];

        let px = cx as f32 * self.text.cell.x;
        let py = cy as f32 * self.text.cell.y;

        self.bg.instances.push(BgInstance {
            position: [px, py],
            color: cursor_bg_rgba,
        });

        let cursor_fg_color = cell_under_cursor.bg;
        let cursor_fg_rgba = [
            cursor_fg_color.0 as f32 / 255.0,
            cursor_fg_color.1 as f32 / 255.0,
            cursor_fg_color.2 as f32 / 255.0,
            1.0,
        ];

        self.text.brush.queue(Section {
            screen_position: (px, py),
            text: vec![
                Text::new("\u{2588}")
                    .with_color(cursor_bg_rgba)
                    .with_scale(CELL_H),
            ],
            ..Section::default()
        });

        self.text.brush.queue(Section {
            screen_position: (px, py),
            text: vec![
                Text::new(&cell_under_cursor.ch.to_string())
                    .with_color(cursor_fg_rgba)
                    .with_scale(CELL_H),
            ],
            ..Section::default()
        });
    }

    /// Current pixel dimensions of the swap-chain surface
    pub fn surface_size(&self) -> (u32, u32) {
        (self.gpu.config.width, self.gpu.config.height)
    }

    /// How many monospace cells fit on screen right now
    pub fn grid_size(&self) -> (usize, usize) {
        let (w_px, h_px) = self.surface_size();
        let (cell_w, cell_h) = self.cell_size();

        ((w_px / cell_w) as usize, (h_px / cell_h) as usize)
    }
}

impl GpuState {
    async fn new(window: &Window) -> Self {
        let instance = Instance::default();

        let surface = unsafe {
            std::mem::transmute::<Surface<'_>, Surface<'static>>(
                instance.create_surface(window).unwrap(),
            )
        };

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable adapter");

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default(), None)
            .await
            .unwrap();

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = select_format(&caps);

        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode: PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Self {
            surface,
            device,
            queue,
            config,
        }
    }
}

impl TextRenderer {
    fn new(device: &Device, format: TextureFormat) -> Self {
        let font = FontArc::try_from_slice(FONT_BYTES).expect("font");

        let scale = ab_glyph::PxScale::from(CELL_H);

        let scaled_font = font.as_scaled(scale);

        let glyph_id = scaled_font.glyph_id(' ');
        let cell_w = scaled_font.h_advance(glyph_id);

        let brush = GlyphBrushBuilder::using_font(font).build(device, format);

        Self {
            brush,
            staging_belt: StagingBelt::new(STAGING_CHUNK.try_into().unwrap()),
            cell: Vec2::new(cell_w, CELL_H),
        }
    }
}

fn select_format(caps: &SurfaceCapabilities) -> TextureFormat {
    caps.formats
        .iter()
        .copied()
        .find(TextureFormat::is_srgb)
        .unwrap_or(caps.formats[0])
}

impl BgRenderer {
    fn new(
        device: &Device,
        format: TextureFormat,
        (screen_w, screen_h): (u32, u32),
        (cell_w, cell_h): (f32, f32),
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bg.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("bg.wgsl").into()),
        });

        let globals_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Globals Buffer"),
            contents: bytemuck::cast_slice(&[screen_w as f32, screen_h as f32, cell_w, cell_h]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let globals_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
                label: Some("globals_bind_group_layout"),
            });

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &globals_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
            label: Some("globals_bind_group"),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Bg Pipeline Layout"),
            bind_group_layouts: &[&globals_bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Bg Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[BgVertex::desc(), BgInstance::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            cache: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Bg Vertex Buffer"),
            contents: bytemuck::cast_slice(BG_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Bg Instance Buffer"),
            size: std::mem::size_of::<BgInstance>() as u64 * 10_000,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            vertex_buffer,
            instances: Vec::with_capacity(10_000),
            instance_buffer,
            globals_bind_group,
            globals_buffer,
        }
    }
}
