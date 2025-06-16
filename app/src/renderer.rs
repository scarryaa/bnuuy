use crate::{config::Config, terminal::TerminalState};
use glam::Vec2;
use screen_grid::{CellFlags, Rgb, Row};
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

#[derive(Debug, Clone)]
struct TextRun {
    text: String,
    x: f32,
    color: [f32; 4],
    is_italic: bool,
}

/// Render cache - holds "ingredients"
#[derive(Debug, Default, Clone)]
struct CachedRow {
    bg_instances: Vec<BgInstance>,
    underline_instances: Vec<UnderlineInstance>,
    undercurl_instances: Vec<UndercurlInstance>,
    text_runs: Vec<TextRun>,
}

#[derive(Debug)]
struct RenderCache {
    rows: Vec<CachedRow>,
}

impl RenderCache {
    fn new(rows: usize) -> Self {
        Self {
            rows: vec![CachedRow::default(); rows],
        }
    }

    fn resize(&mut self, rows: usize) {
        self.rows.resize(rows, CachedRow::default());
    }
}

pub struct Renderer {
    pub window: Arc<Window>,
    gpu: GpuState,

    vertex_buffer: Buffer,
    globals_buffer: Buffer,
    globals_bind_group: BindGroup,

    text: TextRenderer,
    bg: BgRenderer,
    underline: UnderlineRenderer,
    undercurl: UndercurlRenderer,

    cache: RenderCache,

    pub last_mouse_pos: (f32, f32),
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

        let (_cell_w, cell_h) = (text.cell.x as u32, text.cell.y as u32);
        let initial_rows = (gpu.config.height / cell_h) as usize;
        let cache = RenderCache::new(initial_rows);

        let vertex_buffer = gpu.device.create_buffer_init(&util::BufferInitDescriptor {
            label: Some("Shared Vertex Buffer"),
            contents: bytemuck::cast_slice(BG_VERTICES),
            usage: BufferUsages::VERTEX,
        });

        let globals_buffer = gpu.device.create_buffer(&BufferDescriptor {
            label: Some("Shared Globals Buffer"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_bind_group_layout =
            gpu.device
                .create_bind_group_layout(&BindGroupLayoutDescriptor {
                    label: Some("Shared Globals BGL"),
                    entries: &[BindGroupLayoutEntry {
                        binding: 0,
                        visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let globals_bind_group = gpu.device.create_bind_group(&BindGroupDescriptor {
            layout: &globals_bind_group_layout,
            entries: &[BindGroupEntry {
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
            cache,
            vertex_buffer,
            globals_buffer,
            globals_bind_group,
            last_mouse_pos: (0.0, 0.0),
            config,
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w, h) == (self.gpu.config.width, self.gpu.config.height) {
            return;
        }

        self.gpu.config.width = w;
        self.gpu.config.height = h;
        self.gpu
            .surface
            .configure(&self.gpu.device, &self.gpu.config);

        let (_, grid_rows) = self.grid_size();
        self.cache.resize(grid_rows);
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

    pub fn render(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
    ) {
        if !term.is_dirty && selection.is_none() {
            // Do nothing!
        }

        self.text.staging_belt.recall();
        let frame = match self.gpu.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.resize(self.gpu.config.width, self.gpu.config.height);
                return;
            }
            Err(e) => {
                log::error!("surface: {e:?}");
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
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

        self.update_cache_and_prepare_buffers(term, selection);

        // Main render pass
        {
            let mut rpass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Main Render Pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: srgb_to_linear(self.config.colors.background.0) as f64,
                            g: srgb_to_linear(self.config.colors.background.1) as f64,
                            b: srgb_to_linear(self.config.colors.background.2) as f64,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_bind_group(0, &self.globals_bind_group, &[]);
            rpass.set_vertex_buffer(0, self.vertex_buffer.slice(..));

            if !self.bg.instances.is_empty() {
                rpass.set_pipeline(&self.bg.pipeline);
                rpass.set_vertex_buffer(1, self.bg.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..self.bg.instances.len() as u32,
                );
            }

            if !self.underline.instances.is_empty() {
                rpass.set_pipeline(&self.underline.pipeline);
                rpass.set_vertex_buffer(1, self.underline.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..self.underline.instances.len() as u32,
                );
            }

            if !self.undercurl.instances.is_empty() {
                rpass.set_pipeline(&self.undercurl.pipeline);
                rpass.set_vertex_buffer(1, self.undercurl.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..self.undercurl.instances.len() as u32,
                );
            }
        }

        for (y, cached_row) in self.cache.rows.iter().enumerate() {
            let y_pos = y as f32 * self.text.cell.y;

            for run in &cached_row.text_runs {
                let section = Section {
                    screen_position: (run.x, y_pos),
                    text: vec![
                        Text::new(&run.text)
                            .with_color(run.color)
                            .with_scale(self.config.font_size),
                    ],
                    ..Default::default()
                };

                if run.is_italic {
                    self.text.brush_italic.queue(section);
                } else {
                    self.text.brush_regular.queue(section);
                }
            }
        }

        self.queue_cursor(term);

        self.text
            .brush_regular
            .draw_queued(
                &self.gpu.device,
                &mut self.text.staging_belt,
                &mut encoder,
                &view,
                width,
                height,
            )
            .unwrap();
        self.text
            .brush_italic
            .draw_queued(
                &self.gpu.device,
                &mut self.text.staging_belt,
                &mut encoder,
                &view,
                width,
                height,
            )
            .unwrap();
        self.text.staging_belt.finish();

        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();

        term.clear_dirty();
    }

    fn update_cache_and_prepare_buffers(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
    ) {
        let (_grid_cols, grid_rows) = self.grid_size();
        let full_redraw = term.grid.full_redraw_needed || term.scroll_offset > 0;

        // Only process rows that have changed content
        let dirty_rows: Vec<usize> = if full_redraw {
            (0..grid_rows).collect()
        } else {
            (0..grid_rows)
                .filter(|&y| {
                    term.grid
                        .get_display_row(y, term.scroll_offset)
                        .map_or(true, |r| r.is_dirty)
                })
                .collect()
        };

        for y in dirty_rows {
            if let Some(grid_row) = term.grid.get_display_row(y, term.scroll_offset) {
                if let Some(cached_row) = self.cache.rows.get_mut(y) {
                    Self::process_row(grid_row, cached_row, &self.text.cell, self.config.font_size);
                }
            }
        }

        let selection_bg_instances = self.prepare_selection_bg(selection, term);

        // Clear the final buffers
        self.bg.instances.clear();
        self.underline.instances.clear();
        self.undercurl.instances.clear();

        for (y, cached_row) in self.cache.rows.iter().enumerate() {
            let y_pos = y as f32 * self.text.cell.y;

            self.bg
                .instances
                .extend(cached_row.bg_instances.iter().map(|inst| BgInstance {
                    position: [inst.position[0], y_pos],
                    color: inst.color,
                }));
            self.underline
                .instances
                .extend(
                    cached_row
                        .underline_instances
                        .iter()
                        .map(|inst| UnderlineInstance {
                            position: [inst.position[0], y_pos],
                            color: inst.color,
                        }),
                );
            self.undercurl
                .instances
                .extend(
                    cached_row
                        .undercurl_instances
                        .iter()
                        .map(|inst| UndercurlInstance {
                            position: [inst.position[0], y_pos],
                            color: inst.color,
                        }),
                );
        }

        self.bg.instances.extend_from_slice(&selection_bg_instances);

        // Resize and write the buffers to the GPU
        self.bg.resize_and_write(&self.gpu.device, &self.gpu.queue);
        self.underline
            .resize_and_write(&self.gpu.device, &self.gpu.queue);
        self.undercurl
            .resize_and_write(&self.gpu.device, &self.gpu.queue);
    }

    /// Helper to process selection bg
    fn prepare_selection_bg(
        &self,
        selection: Option<((usize, usize), (usize, usize))>,
        term: &TerminalState,
    ) -> Vec<BgInstance> {
        let mut instances = Vec::new();
        let (start_pos, end_pos) = match selection {
            Some((start, end)) => (start, end),
            None => return instances,
        };

        let (start, end) =
            if start_pos.1 < end_pos.1 || (start_pos.1 == end_pos.1 && start_pos.0 <= end_pos.0) {
                (start_pos, end_pos)
            } else {
                (end_pos, start_pos)
            };
        let (start_col, start_row) = start;
        let (end_col, end_row) = end;

        let cell_size = Vec2::new(self.text.cell.x, self.text.cell.y);
        let selection_color = [
            srgb_to_linear(120),
            srgb_to_linear(120),
            srgb_to_linear(120),
            0.5,
        ];

        for y in start_row..=end_row {
            if term.grid.get_display_row(y, term.scroll_offset).is_some() {
                let line_start = if y == start_row { start_col } else { 0 };
                let line_end = if y == end_row {
                    end_col
                } else {
                    term.grid.cols
                };

                for x in line_start..line_end {
                    instances.push(BgInstance {
                        position: [x as f32 * cell_size.x, y as f32 * cell_size.y],
                        color: selection_color,
                    });
                }
            }
        }

        instances
    }

    fn process_row(grid_row: &Row, cached_row: &mut CachedRow, cell_size: &Vec2, _font_size: f32) {
        cached_row.bg_instances.clear();
        cached_row.underline_instances.clear();
        cached_row.undercurl_instances.clear();
        cached_row.text_runs.clear();

        let mut current_run_text = String::new();
        let mut current_run_attrs: Option<(Rgb, CellFlags)> = None;
        let mut current_run_start_x: usize = 0;

        for (x, cell) in grid_row.cells.iter().enumerate() {
            cached_row.bg_instances.push(BgInstance {
                position: [x as f32 * cell_size.x, 0.0],
                color: [
                    srgb_to_linear(cell.bg.0),
                    srgb_to_linear(cell.bg.1),
                    srgb_to_linear(cell.bg.2),
                    1.0,
                ],
            });

            let fg_color = [
                srgb_to_linear(cell.fg.0),
                srgb_to_linear(cell.fg.1),
                srgb_to_linear(cell.fg.2),
                1.0,
            ];

            if cell.flags.contains(CellFlags::UNDERLINE) {
                cached_row.underline_instances.push(UnderlineInstance {
                    position: [x as f32 * cell_size.x, 0.0],
                    color: fg_color,
                });
            }

            if cell.flags.contains(CellFlags::UNDERCURL) {
                cached_row.undercurl_instances.push(UndercurlInstance {
                    position: [x as f32 * cell_size.x, 0.0],
                    color: fg_color,
                });
            }

            let text_attrs = (cell.fg, cell.flags);
            if cell.ch != ' ' && Some(text_attrs) == current_run_attrs {
                current_run_text.push(cell.ch);
            } else {
                if !current_run_text.is_empty() {
                    let (fg, flags) = current_run_attrs.unwrap();

                    let mut final_color = [
                        srgb_to_linear(fg.0),
                        srgb_to_linear(fg.1),
                        srgb_to_linear(fg.2),
                        1.0,
                    ];

                    if flags.contains(CellFlags::FAINT) {
                        // Make the color dimmer
                        final_color[0] *= 0.66;
                        final_color[1] *= 0.66;
                        final_color[2] *= 0.66;
                    }
                    if flags.contains(CellFlags::BOLD) {
                        // Make the color brighter
                        final_color[0] = (final_color[0] * 1.5).min(1.0);
                        final_color[1] = (final_color[1] * 1.5).min(1.0);
                        final_color[2] = (final_color[2] * 1.5).min(1.0);
                    }

                    let text_to_store = std::mem::take(&mut current_run_text);

                    cached_row.text_runs.push(TextRun {
                        text: text_to_store,
                        x: current_run_start_x as f32 * cell_size.x,
                        color: final_color,
                        is_italic: flags.contains(CellFlags::ITALIC),
                    });
                }

                if cell.ch != ' ' {
                    current_run_start_x = x;
                    current_run_attrs = Some(text_attrs);
                    current_run_text.push(cell.ch);
                } else {
                    current_run_attrs = None;
                }
            }
        }

        // Process final run
        if !current_run_text.is_empty() {
            let (fg, flags) = current_run_attrs.unwrap();

            let mut final_color = [
                srgb_to_linear(fg.0),
                srgb_to_linear(fg.1),
                srgb_to_linear(fg.2),
                1.0,
            ];
            if flags.contains(CellFlags::FAINT) {
                final_color[0] *= 0.66;
                final_color[1] *= 0.66;
                final_color[2] *= 0.66;
            }
            if flags.contains(CellFlags::BOLD) {
                final_color[0] = (final_color[0] * 1.5).min(1.0);
                final_color[1] = (final_color[1] * 1.5).min(1.0);
                final_color[2] = (final_color[2] * 1.5).min(1.0);
            }

            cached_row.text_runs.push(TextRun {
                text: current_run_text,
                x: current_run_start_x as f32 * cell_size.x,
                color: final_color,
                is_italic: flags.contains(CellFlags::ITALIC),
            });
        }
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

    fn resize_and_write(&mut self, device: &Device, queue: &Queue) {
        let required_instances = self.instances.len() as u64;

        if required_instances > self.instance_capacity {
            self.instance_capacity = (required_instances as f32 * 1.5) as u64;
            self.instance_buffer = device.create_buffer(&BufferDescriptor {
                label: Some("Bg Instance Buffer (Resized)"),
                size: std::mem::size_of::<BgInstance>() as u64 * self.instance_capacity,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
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

    fn resize_and_write(&mut self, device: &Device, queue: &Queue) {
        let required_instances = self.instances.len() as u64;

        if required_instances > self.instance_capacity {
            self.instance_capacity = (required_instances as f32 * 1.5) as u64;
            self.instance_buffer = device.create_buffer(&BufferDescriptor {
                label: Some("Undercurl Instance Buffer (Resized)"),
                size: std::mem::size_of::<UndercurlInstance>() as u64 * self.instance_capacity,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
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

    fn resize_and_write(&mut self, device: &Device, queue: &Queue) {
        let required_instances = self.instances.len() as u64;

        if required_instances > self.instance_capacity {
            self.instance_capacity = (required_instances as f32 * 1.5) as u64;
            self.instance_buffer = device.create_buffer(&BufferDescriptor {
                label: Some("Underline Instance Buffer (Resized)"),
                size: std::mem::size_of::<UnderlineInstance>() as u64 * self.instance_capacity,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
    }
}
