use crate::{config::Config, terminal::TerminalState};
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

/// Converts a color from sRGB space to linear space.
fn srgb_to_linear(c: u8) -> f32 {
    (c as f32 / 255.0).powf(2.2)
}

/// Compile-time embedded font
const FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/fonts/HackNerdFontMono-Regular.ttf"
));

/// Compile-time embedded italic font
const FONT_BYTES_ITALIC: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/fonts/HackNerdFontMono-Italic.ttf"
));

const STAGING_CHUNK: usize = 1 << 16;

pub struct Renderer {
    pub window: Arc<Window>,
    gpu: GpuState,

    vertex_buffer: Buffer,
    globals_buffer: Buffer,
    globals_bind_group: BindGroup,
    globals_bind_group_layout: BindGroupLayout,

    text: TextRenderer,
    bg: BgRenderer,
    underline: UnderlineRenderer,
    undercurl: UndercurlRenderer,

    pub last_mouse_pos: (f32, f32), // in px
    config: Arc<Config>,
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
struct UndercurlInstance {
    position: [f32; 2], // top-left corner of the cell, in px
    color: [f32; 4],    // color of the undercurl
}

impl UndercurlInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![3 => Float32x2, 4 => Float32x4];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct UnderlineInstance {
    position: [f32; 2], // top-left corner of the cell, in px
    color: [f32; 4],    // color of the underline
}

impl UnderlineInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![5 => Float32x2, 6 => Float32x4];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

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

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    screen_size: [f32; 2],
    cell_size: [f32; 2],
    _padding: f32,
}

#[derive(Debug)]
struct BgRenderer {
    pipeline: RenderPipeline,
    instances: Vec<BgInstance>,
    instance_buffer: Buffer,
    instance_capacity: u64,
}

#[derive(Debug)]
struct UndercurlRenderer {
    pipeline: RenderPipeline,
    instances: Vec<UndercurlInstance>,
    instance_buffer: Buffer,
    instance_capacity: u64,
}

#[derive(Debug)]
struct UnderlineRenderer {
    pipeline: RenderPipeline,
    instances: Vec<UnderlineInstance>,
    instance_buffer: Buffer,
    instance_capacity: u64,
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
    brush_regular: GlyphBrush<()>,
    brush_italic: GlyphBrush<()>,
    staging_belt: StagingBelt,
    cell: Vec2,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, config: Arc<Config>) -> Self {
        let gpu = GpuState::new(window.as_ref()).await;
        let text = TextRenderer::new(&gpu.device, gpu.config.format, config.font_size);

        let vertex_buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Shared Vertex Buffer"),
                contents: bytemuck::cast_slice(BG_VERTICES),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let globals_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Shared Globals Buffer"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Shared Globals BGL"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let globals_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &globals_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
            label: Some("Shared Globals BG"),
        });

        let bg = BgRenderer::new(&gpu.device, gpu.config.format, &globals_bind_group_layout);
        let undercurl =
            UndercurlRenderer::new(&gpu.device, gpu.config.format, &globals_bind_group_layout);
        let underline =
            UnderlineRenderer::new(&gpu.device, gpu.config.format, &globals_bind_group_layout);

        Self {
            window,
            gpu,
            text,
            bg,
            underline,
            undercurl,
            vertex_buffer,
            globals_buffer,
            globals_bind_group,
            globals_bind_group_layout,
            last_mouse_pos: (0.0, 0.0),
            config,
        }
    }

    pub fn pixels_to_grid(&self, pos: (f32, f32)) -> (usize, usize) {
        let (cell_w, cell_h) = self.cell_size();
        let col = (pos.0 / cell_w as f32).floor() as usize;
        let row = (pos.1 / cell_h as f32).floor() as usize;
        let (grid_cols, grid_rows) = self.grid_size();

        (col.min(grid_cols - 1), row.min(grid_rows - 1))
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
    }

    pub fn render(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
    ) {
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
                label: Some("Terminal Encoder"),
            });

        let (width, height) = self.surface_size();
        let (cell_w, cell_h) = self.cell_size();
        let globals = Globals {
            screen_size: [width as f32, height as f32],
            cell_size: [cell_w as f32, cell_h as f32],
            _padding: 0.0,
        };

        self.gpu
            .queue
            .write_buffer(&self.globals_buffer, 0, bytemuck::cast_slice(&[globals]));

        self.prepare_render_data(term, selection);

        // Resize BG buffer if needed
        let required_bg_instances = self.bg.instances.len() as u64;
        if required_bg_instances > self.bg.instance_capacity {
            let new_capacity = (required_bg_instances as f32 * 1.5) as u64;
            self.bg.instance_buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Bg Instance Buffer (Resized)"),
                size: std::mem::size_of::<BgInstance>() as u64 * new_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.bg.instance_capacity = new_capacity;
        }

        // Resize Underline buffer if needed
        let required_ul_instances = self.underline.instances.len() as u64;
        if required_ul_instances > self.underline.instance_capacity {
            let new_capacity = (required_ul_instances as f32 * 1.5) as u64;
            self.underline.instance_buffer =
                self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Underline Instance Buffer (Resized)"),
                    size: std::mem::size_of::<UnderlineInstance>() as u64 * new_capacity,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            self.underline.instance_capacity = new_capacity;
        }

        // Resize Undercurl buffer if needed
        let required_uc_instances = self.undercurl.instances.len() as u64;
        if required_uc_instances > self.undercurl.instance_capacity {
            let new_capacity = (required_uc_instances as f32 * 1.5) as u64;
            self.undercurl.instance_buffer =
                self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Undercurl Instance Buffer (Resized)"),
                    size: std::mem::size_of::<UndercurlInstance>() as u64 * new_capacity,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            self.undercurl.instance_capacity = new_capacity;
        }

        self.gpu.queue.write_buffer(
            &self.bg.instance_buffer,
            0,
            bytemuck::cast_slice(&self.bg.instances),
        );
        self.gpu.queue.write_buffer(
            &self.underline.instance_buffer,
            0,
            bytemuck::cast_slice(&self.underline.instances),
        );
        self.gpu.queue.write_buffer(
            &self.undercurl.instance_buffer,
            0,
            bytemuck::cast_slice(&self.undercurl.instances),
        );

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Main Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: srgb_to_linear(self.config.colors.background.0) as f64,
                            g: srgb_to_linear(self.config.colors.background.1) as f64,
                            b: srgb_to_linear(self.config.colors.background.2) as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_bind_group(0, &self.globals_bind_group, &[]);
            rpass.set_vertex_buffer(0, self.vertex_buffer.slice(..));

            // Draw Background
            if !self.bg.instances.is_empty() {
                rpass.set_pipeline(&self.bg.pipeline);
                rpass.set_vertex_buffer(1, self.bg.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..self.bg.instances.len() as u32,
                );
            }

            // Draw Underlines
            if !self.underline.instances.is_empty() {
                rpass.set_pipeline(&self.underline.pipeline);
                rpass.set_vertex_buffer(1, self.underline.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..self.underline.instances.len() as u32,
                );
            }

            // Draw Undercurls
            if !self.undercurl.instances.is_empty() {
                rpass.set_pipeline(&self.undercurl.pipeline);
                rpass.set_vertex_buffer(1, self.undercurl.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..self.undercurl.instances.len() as u32,
                );
            }
        }

        self.text
            .brush_regular
            .draw_queued(
                &self.gpu.device,
                &mut self.text.staging_belt,
                &mut encoder,
                &view,
                self.gpu.config.width,
                self.gpu.config.height,
            )
            .expect("draw regular glyphs");

        self.text
            .brush_italic
            .draw_queued(
                &self.gpu.device,
                &mut self.text.staging_belt,
                &mut encoder,
                &view,
                self.gpu.config.width,
                self.gpu.config.height,
            )
            .expect("draw italic glyphs");

        self.text.staging_belt.finish();
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    fn prepare_render_data(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
    ) {
        let (grid_cols, grid_rows) = self.grid_size();
        let cell_size = Vec2::new(self.text.cell.x, self.text.cell.y);

        let normalized_selection = selection.map(|(start, end)| {
            let start_col = start.0.min(end.0);
            let start_row = start.1.min(end.1);
            let end_col = start.0.max(end.0);
            let end_row = start.1.max(end.1);
            (start_col, start_row, end_col, end_row)
        });

        self.bg.instances.clear();
        self.underline.instances.clear();
        self.undercurl.instances.clear();

        for y in 0..grid_rows {
            let term_row = term.grid.get_display_row(y, term.scroll_offset);

            let mut current_run_text = String::new();
            let mut current_run_attrs: Option<(Rgb, CellFlags)> = None;
            let mut current_run_start_x: usize = 0;

            for x in 0..grid_cols {
                let cell_to_draw = term_row
                    .and_then(|row| row.cells.get(x))
                    .cloned()
                    .unwrap_or_default();

                let is_selected = normalized_selection
                    .map(|(sc, sr, ec, er)| x >= sc && x <= ec && y >= sr && y <= er)
                    .unwrap_or(false);

                let mut bg_color = [
                    srgb_to_linear(cell_to_draw.bg.0),
                    srgb_to_linear(cell_to_draw.bg.1),
                    srgb_to_linear(cell_to_draw.bg.2),
                    1.0,
                ];

                if is_selected {
                    bg_color = [
                        srgb_to_linear(120),
                        srgb_to_linear(120),
                        srgb_to_linear(120),
                        0.5,
                    ];
                }

                self.bg.instances.push(BgInstance {
                    position: [x as f32 * cell_size.x, y as f32 * cell_size.y],
                    color: bg_color,
                });

                let fg_color = cell_to_draw.fg;
                let underline_color = [
                    srgb_to_linear(fg_color.0),
                    srgb_to_linear(fg_color.1),
                    srgb_to_linear(fg_color.2),
                    1.0,
                ];

                if cell_to_draw.flags.contains(CellFlags::UNDERLINE) {
                    self.underline.instances.push(UnderlineInstance {
                        position: [x as f32 * cell_size.x, y as f32 * cell_size.y],
                        color: underline_color,
                    });
                }

                if cell_to_draw.flags.contains(CellFlags::UNDERCURL) {
                    self.undercurl.instances.push(UndercurlInstance {
                        position: [x as f32 * cell_size.x, y as f32 * cell_size.y],
                        color: underline_color,
                    });
                }

                let text_attrs = (cell_to_draw.fg, cell_to_draw.flags);
                let is_glyph_with_same_style =
                    cell_to_draw.ch != ' ' && Some(text_attrs) == current_run_attrs;

                if is_glyph_with_same_style {
                    current_run_text.push(cell_to_draw.ch);
                } else {
                    if !current_run_text.is_empty() {
                        let (fg, flags) = current_run_attrs.unwrap();
                        let mut rgba = [
                            srgb_to_linear(fg.0),
                            srgb_to_linear(fg.1),
                            srgb_to_linear(fg.2),
                            1.0,
                        ];
                        if flags.contains(CellFlags::FAINT) {
                            for chan in &mut rgba[0..3] {
                                *chan *= 0.5;
                            }
                        }

                        let section = Section {
                            screen_position: (
                                current_run_start_x as f32 * cell_size.x,
                                y as f32 * cell_size.y,
                            ),
                            text: vec![
                                Text::new(&current_run_text)
                                    .with_color(rgba)
                                    .with_scale(self.config.font_size),
                            ],
                            ..Section::default()
                        };

                        if flags.contains(CellFlags::ITALIC) {
                            self.text.brush_italic.queue(section);
                        } else {
                            self.text.brush_regular.queue(section);
                        }
                    }
                    current_run_text.clear();

                    if cell_to_draw.ch != ' ' {
                        current_run_start_x = x;
                        current_run_attrs = Some(text_attrs);
                        current_run_text.push(cell_to_draw.ch);
                    } else {
                        current_run_attrs = None;
                    }
                }
            }

            if !current_run_text.is_empty() {
                let (fg, flags) = current_run_attrs.unwrap();
                let mut rgba = [
                    srgb_to_linear(fg.0),
                    srgb_to_linear(fg.1),
                    srgb_to_linear(fg.2),
                    1.0,
                ];
                if flags.contains(CellFlags::FAINT) {
                    for chan in &mut rgba[0..3] {
                        *chan *= 0.5;
                    }
                }
                let section = Section {
                    screen_position: (
                        current_run_start_x as f32 * cell_size.x,
                        y as f32 * cell_size.y,
                    ),
                    text: vec![
                        Text::new(&current_run_text)
                            .with_color(rgba)
                            .with_scale(self.config.font_size),
                    ],
                    ..Section::default()
                };
                if flags.contains(CellFlags::ITALIC) {
                    self.text.brush_italic.queue(section);
                } else {
                    self.text.brush_regular.queue(section);
                }
            }
        }
        self.queue_cursor(term);
    }

    fn queue_cursor(&mut self, term: &TerminalState) {
        // Don't render cursor if it should be hidden
        if !term.cursor_visible {
            return;
        }

        // Don't render cursor if scrolled back
        if term.scroll_offset != 0 {
            return;
        }

        let (cx, cy) = (term.grid.cur_x, term.grid.cur_y);

        let cell_under_cursor = term
            .grid
            .visible_row(cy)
            .and_then(|r| r.cells.get(cx))
            .cloned()
            .unwrap_or_default();

        let cursor_bg_color = cell_under_cursor.fg;
        let cursor_bg_rgba = [
            srgb_to_linear(cursor_bg_color.0),
            srgb_to_linear(cursor_bg_color.1),
            srgb_to_linear(cursor_bg_color.2),
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
            srgb_to_linear(cursor_fg_color.0),
            srgb_to_linear(cursor_fg_color.1),
            srgb_to_linear(cursor_fg_color.2),
            1.0,
        ];

        self.text.brush_regular.queue(Section {
            screen_position: (px, py),
            text: vec![
                Text::new("\u{2588}")
                    .with_color(cursor_bg_rgba)
                    .with_scale(self.config.font_size),
            ],
            ..Section::default()
        });

        self.text.brush_regular.queue(Section {
            screen_position: (px, py),
            text: vec![
                Text::new(&cell_under_cursor.ch.to_string())
                    .with_color(cursor_fg_rgba)
                    .with_scale(self.config.font_size),
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
            present_mode: PresentMode::Immediate,
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
    fn new(device: &Device, format: TextureFormat, font_size: f32) -> Self {
        let font_regular = FontArc::try_from_slice(FONT_BYTES).expect("load regular font");
        let brush_regular =
            GlyphBrushBuilder::using_font(font_regular.clone()).build(device, format);

        let font_italic = FontArc::try_from_slice(FONT_BYTES_ITALIC).expect("load italic font");
        let brush_italic = GlyphBrushBuilder::using_font(font_italic).build(device, format);

        let scale = ab_glyph::PxScale::from(font_size);
        let scaled_font = font_regular.as_scaled(scale);
        let glyph_id = scaled_font.glyph_id(' ');
        let cell_w = scaled_font.h_advance(glyph_id).floor();

        Self {
            brush_regular,
            brush_italic,
            staging_belt: StagingBelt::new(STAGING_CHUNK.try_into().unwrap()),
            cell: Vec2::new(cell_w, font_size),
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
    fn new(device: &Device, format: TextureFormat, globals_layout: &BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bg.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bg.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Bg Pipeline Layout"),
            bind_group_layouts: &[globals_layout],
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            cache: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let initial_capacity = 10_000;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Bg Instance Buffer"),
            size: std::mem::size_of::<BgInstance>() as u64 * initial_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            instances: Vec::with_capacity(initial_capacity as usize),
            instance_buffer,
            instance_capacity: initial_capacity,
        }
    }
}

impl UndercurlRenderer {
    fn new(device: &Device, format: TextureFormat, globals_layout: &BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("undercurl.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/undercurl.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Undercurl Pipeline Layout"),
            bind_group_layouts: &[globals_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Undercurl Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[BgVertex::desc(), UndercurlInstance::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            cache: None,
            multiview: None,
        });

        let initial_capacity = 2_000;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Undercurl Instance Buffer"),
            size: std::mem::size_of::<UndercurlInstance>() as u64 * initial_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            instances: Vec::with_capacity(initial_capacity as usize),
            instance_buffer,
            instance_capacity: initial_capacity,
        }
    }
}

impl UnderlineRenderer {
    fn new(device: &Device, format: TextureFormat, globals_layout: &BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("underline.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/underline.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Underline Pipeline Layout"),
            bind_group_layouts: &[globals_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Underline Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[BgVertex::desc(), UnderlineInstance::desc()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            cache: None,
            multiview: None,
        });

        let initial_capacity = 2_000;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Underline Instance Buffer"),
            size: std::mem::size_of::<UnderlineInstance>() as u64 * initial_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            instances: Vec::with_capacity(initial_capacity as usize),
            instance_buffer,
            instance_capacity: initial_capacity,
        }
    }
}
