use crate::{config::Config, terminal::TerminalState};
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, Style, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use lru::LruCache;
use screen_grid::CellFlags;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    num::NonZeroUsize,
    sync::Arc,
};
use wgpu::{util::DeviceExt, *};
use winit::window::{Window, WindowId};

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

pub struct Renderer {
    pub window: Arc<Window>,
    gpu: GpuState,

    vertex_buffer: wgpu::Buffer,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    bg: BgRenderer,
    underline: UnderlineRenderer,
    undercurl: UndercurlRenderer,

    bg_clear_color: wgpu::Color,

    text_cache: LruCache<u64, Buffer>,
    bg_cache: LruCache<u64, Vec<BgInstance>>,
    underline_cache: LruCache<u64, Vec<UnderlineInstance>>,
    undercurl_cache: LruCache<u64, Vec<UndercurlInstance>>,
    cache: Cache,

    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: glyphon::TextRenderer,

    cell_size: (f32, f32),

    pub last_mouse_pos: (f32, f32),
    config: Arc<Config>,
    default_attrs: Attrs<'static>,
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
    color: [u8; 4],     // color of the undercurl
}

impl UndercurlInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![3 => Float32x2, 4 => Unorm8x4];

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
    color: [u8; 4],     // color of the underline
}

impl UnderlineInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![5 => Float32x2, 6 => Unorm8x4];

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
    color: [u8; 4],
}

impl BgInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![1 => Float32x2, 2 => Unorm8x4];

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
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
}

#[derive(Debug)]
struct UndercurlRenderer {
    pipeline: RenderPipeline,
    instances: Vec<UndercurlInstance>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
}

#[derive(Debug)]
struct UnderlineRenderer {
    pipeline: RenderPipeline,
    instances: Vec<UnderlineInstance>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
}

#[derive(Debug)]
struct GpuState {
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,
}

/// Helper function to perform text shaping and caching
fn prepare_text<'a>(
    text_cache: &'a mut LruCache<u64, Buffer>,
    font_system: &mut FontSystem,
    config: &Config,
    default_attrs: &Attrs<'static>,
    cell_size: (f32, f32),
    term: &TerminalState,
    surface_size: (u32, u32),
    grid_size: (usize, usize),
) -> Vec<TextArea<'a>> {
    let (grid_cols, grid_rows) = grid_size;
    let (width, height) = surface_size;
    let cursor_visible = term.cursor_visible && term.scroll_offset == 0;

    let mut cache_misses = Vec::new();
    for y in 0..grid_rows {
        if let Some(grid_row) = term.grid().get_display_row(y, term.scroll_offset) {
            let mut hasher = DefaultHasher::new();
            grid_row.hash(&mut hasher);
            if cursor_visible && y == term.grid().cur_y {
                term.grid().cur_x.hash(&mut hasher);
            }
            let row_hash = hasher.finish();
            if !text_cache.contains(&row_hash) {
                cache_misses.push((row_hash, y, grid_row.clone()));
            }
        }
    }

    for (row_hash, y, grid_row) in cache_misses {
        let mut buffer = Buffer::new(font_system, Metrics::new(config.font_size, cell_size.1));
        buffer.set_size(
            font_system,
            Some(grid_cols as f32 * cell_size.0),
            Some(cell_size.1),
        );

        let mut line_text = String::with_capacity(grid_cols);
        let mut attrs_list = glyphon::AttrsList::new(default_attrs);

        if !grid_row.cells.is_empty() {
            let mut run_start_byte = 0;
            let mut run_start_cell = &grid_row.cells[0];
            let mut run_start_cursor =
                cursor_visible && y == term.grid().cur_y && 0 == term.grid().cur_x;

            for (i, cell) in grid_row.cells.iter().enumerate() {
                let is_cursor = cursor_visible && y == term.grid().cur_y && i == term.grid().cur_x;

                if *cell != *run_start_cell || is_cursor != run_start_cursor {
                    let run_end_byte = line_text.len();
                    if run_end_byte > run_start_byte {
                        let fg = if run_start_cursor {
                            run_start_cell.bg
                        } else {
                            run_start_cell.fg
                        };
                        let mut attrs = default_attrs
                            .clone()
                            .color(glyphon::Color::rgba(fg.0, fg.1, fg.2, 0xFF));
                        if run_start_cell.flags.contains(CellFlags::ITALIC) {
                            attrs = attrs.style(Style::Italic);
                        }
                        if run_start_cell.flags.contains(CellFlags::BOLD) {
                            attrs = attrs.weight(Weight::BOLD);
                        }
                        attrs_list.add_span(run_start_byte..run_end_byte, &attrs);
                    }

                    // Start a new run
                    run_start_byte = run_end_byte;
                    run_start_cell = cell;
                    run_start_cursor = is_cursor;
                }

                line_text.push(cell.ch);
            }

            let run_end_byte = line_text.len();
            if run_end_byte > run_start_byte {
                let fg = if run_start_cursor {
                    run_start_cell.bg
                } else {
                    run_start_cell.fg
                };
                let mut attrs = default_attrs
                    .clone()
                    .color(glyphon::Color::rgba(fg.0, fg.1, fg.2, 0xFF));
                if run_start_cell.flags.contains(CellFlags::ITALIC) {
                    attrs = attrs.style(Style::Italic);
                }
                if run_start_cell.flags.contains(CellFlags::BOLD) {
                    attrs = attrs.weight(Weight::BOLD);
                }
                attrs_list.add_span(run_start_byte..run_end_byte, &attrs);
            }
        }

        buffer.set_text(font_system, &line_text, default_attrs, Shaping::Advanced);
        buffer.lines[0].set_attrs_list(attrs_list);
        buffer.shape_until_scroll(font_system, true);
        text_cache.put(row_hash, buffer);
    }

    let mut text_areas = Vec::with_capacity(grid_rows);
    for y in 0..grid_rows {
        if let Some(grid_row) = term.grid().get_display_row(y, term.scroll_offset) {
            let mut hasher = DefaultHasher::new();
            grid_row.hash(&mut hasher);
            if cursor_visible && y == term.grid().cur_y {
                term.grid().cur_x.hash(&mut hasher);
            }
            let row_hash = hasher.finish();
            if let Some(buffer) = text_cache.peek(&row_hash) {
                text_areas.push(TextArea {
                    buffer,
                    left: 0.0,
                    top: y as f32 * cell_size.1,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: width as i32,
                        bottom: height as i32,
                    },
                    custom_glyphs: &[],
                    default_color: glyphon::Color::rgb(0xFF, 0x00, 0xFF),
                });
            }
        }
    }

    text_areas
}

impl Renderer {
    pub async fn new(window: Arc<Window>, config: Arc<Config>) -> Self {
        let gpu = GpuState::new(window.as_ref()).await;

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&gpu.device);
        let default_attrs = Attrs::new().family(Family::Monospace);

        let mut atlas = TextAtlas::new(&gpu.device, &gpu.queue, &cache, gpu.config.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &gpu.device, MultisampleState::default(), None);

        font_system.db_mut().load_font_data(Vec::from(FONT_BYTES));
        font_system
            .db_mut()
            .load_font_data(Vec::from(FONT_BYTES_ITALIC));
        font_system
            .db_mut()
            .set_monospace_family("Hack Nerd Font Mono");

        let mut buffer = Buffer::new(
            &mut font_system,
            Metrics::new(config.font_size, config.font_size),
        );

        buffer.set_text(&mut font_system, "W", &default_attrs, Shaping::Advanced);

        let cell_w = buffer.layout_runs().next().unwrap().line_w;
        let cell_size = (cell_w, config.font_size);

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

        let text_cache = LruCache::new(NonZeroUsize::new(4000).unwrap());
        let bg_cache = LruCache::new(NonZeroUsize::new(4000).unwrap());
        let underline_cache = LruCache::new(NonZeroUsize::new(4000).unwrap());
        let undercurl_cache = LruCache::new(NonZeroUsize::new(4000).unwrap());

        let bg_clear_color = {
            let (r, g, b) = config.colors.background;
            let srgb_to_linear_f64 = |c: u8| (c as f64 / 255.0).powf(2.2);
            wgpu::Color {
                r: srgb_to_linear_f64(r),
                g: srgb_to_linear_f64(g),
                b: srgb_to_linear_f64(b),
                a: 1.0,
            }
        };

        Self {
            window,
            gpu,
            vertex_buffer,
            globals_buffer,
            globals_bind_group,
            bg_clear_color,
            bg,
            underline,
            undercurl,
            text_cache,
            bg_cache,
            underline_cache,
            undercurl_cache,
            cache,
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            cell_size,
            last_mouse_pos: (0.0, 0.0),
            config,
            default_attrs,
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

    pub fn render(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
        hovered_link_id: Option<u32>,
    ) {
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

        self.prepare_decorations(term, selection, hovered_link_id);

        let grid_size = self.grid_size();

        {
            let Self {
                gpu,
                text_cache,
                font_system,
                atlas,
                swash_cache,
                cache,
                text_renderer,
                config,
                cell_size,
                default_attrs,
                bg,
                underline,
                undercurl,
                vertex_buffer,
                globals_bind_group,
                bg_clear_color,
                ..
            } = self;

            let text_areas = prepare_text(
                text_cache,
                font_system,
                config,
                default_attrs,
                *cell_size,
                term,
                (width, height),
                grid_size,
            );

            let mut viewport = Viewport::new(&gpu.device, cache);
            viewport.update(
                &gpu.queue,
                Resolution {
                    width: gpu.config.width,
                    height: gpu.config.height,
                },
            );

            text_renderer
                .prepare(
                    &gpu.device,
                    &gpu.queue,
                    font_system,
                    atlas,
                    &viewport,
                    text_areas,
                    swash_cache,
                )
                .unwrap();

            let mut rpass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Main Render Pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(*bg_clear_color),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rpass.set_bind_group(0, &*globals_bind_group, &[]);
            rpass.set_vertex_buffer(0, vertex_buffer.slice(..));

            if !bg.instances.is_empty() {
                rpass.set_pipeline(&bg.pipeline);
                rpass.set_vertex_buffer(1, bg.instance_buffer.slice(..));
                rpass.draw(0..BG_VERTICES.len() as u32, 0..bg.instances.len() as u32);
            }

            if !underline.instances.is_empty() {
                rpass.set_pipeline(&underline.pipeline);
                rpass.set_vertex_buffer(1, underline.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..underline.instances.len() as u32,
                );
            }

            if !undercurl.instances.is_empty() {
                rpass.set_pipeline(&undercurl.pipeline);
                rpass.set_vertex_buffer(1, undercurl.instance_buffer.slice(..));
                rpass.draw(
                    0..BG_VERTICES.len() as u32,
                    0..undercurl.instances.len() as u32,
                );
            }

            // Draw the text
            text_renderer.render(atlas, &viewport, &mut rpass).unwrap();
        }

        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        term.clear_dirty();
    }

    /// Prepare background colors and all decorations like underlines and selections
    fn prepare_decorations(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
        hovered_link_id: Option<u32>,
    ) {
        let (_grid_cols, grid_rows) = self.grid_size();
        let cursor_visible = term.cursor_visible && term.scroll_offset == 0;

        // Clear old instance data
        self.bg.instances.clear();
        self.underline.instances.clear();
        self.undercurl.instances.clear();

        // Loop over every visible row
        for y in 0..grid_rows {
            if let Some(grid_row) = term.grid().get_display_row(y, term.scroll_offset) {
                let mut hasher = DefaultHasher::new();
                grid_row.hash(&mut hasher);

                if cursor_visible && y == term.grid().cur_y {
                    term.grid().cur_x.hash(&mut hasher);
                }

                let row_hovered_link_id = if let Some(id) = hovered_link_id {
                    if grid_row.cells.iter().any(|c| c.link_id == Some(id)) {
                        Some(id)
                    } else {
                        None
                    }
                } else {
                    None
                };
                row_hovered_link_id.hash(&mut hasher);

                let row_hash = hasher.finish();

                let cache_hit = self.bg_cache.contains(&row_hash);

                if cache_hit {
                    // Fast path
                    if let Some(cached_bgs) = self.bg_cache.get(&row_hash) {
                        self.bg.instances.extend_from_slice(cached_bgs);
                    }
                    if let Some(cached_underlines) = self.underline_cache.get(&row_hash) {
                        self.underline
                            .instances
                            .extend_from_slice(cached_underlines);
                    }
                    if let Some(cached_undercurls) = self.undercurl_cache.get(&row_hash) {
                        self.undercurl
                            .instances
                            .extend_from_slice(cached_undercurls);
                    }
                } else {
                    // Slow path
                    let mut row_bgs = Vec::with_capacity(grid_row.cells.len());
                    let mut row_underlines = Vec::new();
                    let mut row_undercurls = Vec::new();

                    let y_pos = y as f32 * self.cell_size.1;

                    for (x, cell) in grid_row.cells.iter().enumerate() {
                        let is_cursor =
                            cursor_visible && y == term.grid().cur_y && x == term.grid().cur_x;

                        // Stage the Background
                        let bg_color = if is_cursor { cell.fg } else { cell.bg };
                        row_bgs.push(BgInstance {
                            position: [x as f32 * self.cell_size.0, y_pos],
                            color: [bg_color.0, bg_color.1, bg_color.2, 255],
                        });

                        // Stage Underlines and Undercurls
                        let fg_color = if is_cursor { cell.bg } else { cell.fg };
                        let final_fg_color = [fg_color.0, fg_color.1, fg_color.2, 255];
                        let is_hovered_link =
                            cell.link_id == hovered_link_id && hovered_link_id.is_some();

                        if cell.flags.contains(CellFlags::UNDERLINE) {
                            row_underlines.push(UnderlineInstance {
                                position: [x as f32 * self.cell_size.0, y_pos],
                                color: final_fg_color,
                            });
                        }
                        if cell.flags.contains(CellFlags::UNDERCURL) || is_hovered_link {
                            row_undercurls.push(UndercurlInstance {
                                position: [x as f32 * self.cell_size.0, y_pos],
                                color: final_fg_color,
                            });
                        }
                    }

                    self.bg.instances.extend_from_slice(&row_bgs);
                    self.underline.instances.extend_from_slice(&row_underlines);
                    self.undercurl.instances.extend_from_slice(&row_undercurls);

                    self.bg_cache.put(row_hash, row_bgs);
                    self.underline_cache.put(row_hash, row_underlines);
                    self.undercurl_cache.put(row_hash, row_undercurls);
                }
            }
        }

        // Add selection highlights on top
        let selection_bg_instances = self.prepare_selection_bg(selection, term);
        self.bg.instances.extend_from_slice(&selection_bg_instances);

        // Send everything to the gpu
        self.bg.resize_and_write(&self.gpu.device, &self.gpu.queue);
        self.underline
            .resize_and_write(&self.gpu.device, &self.gpu.queue);
        self.undercurl
            .resize_and_write(&self.gpu.device, &self.gpu.queue);
    }

    pub fn cell_size(&self) -> (u32, u32) {
        (
            self.cell_size.0.ceil() as u32,
            self.cell_size.1.ceil() as u32,
        )
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

        let cell_size = self.cell_size;
        let selection_color = [120, 120, 120, 128];

        for y in start_row..=end_row {
            if term.grid().get_display_row(y, term.scroll_offset).is_some() {
                let line_start = if y == start_row { start_col } else { 0 };
                let line_end = if y == end_row {
                    end_col
                } else {
                    term.grid().cols
                };

                for x in line_start..line_end {
                    instances.push(BgInstance {
                        position: [x as f32 * cell_size.0, y as f32 * cell_size.1],
                        color: selection_color,
                    });
                }
            }
        }

        instances
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
            .request_device(&DeviceDescriptor::default())
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
