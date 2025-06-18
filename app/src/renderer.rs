use crate::{config::Config, terminal::TerminalState};
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport, fontdb,
};
use lru::LruCache;
use screen_grid::{CellFlags, Rgb};
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

    bg_cache: LruCache<u64, Vec<BgInstance>>,
    underline_cache: LruCache<u64, Vec<UnderlineInstance>>,
    undercurl_cache: LruCache<u64, Vec<UndercurlInstance>>,
    cache: Cache,

    atlas: TextAtlas,
    text_renderer: glyphon::TextRenderer,

    last_scroll_offset: usize,
    last_selection: Option<((usize, usize), (usize, usize))>,
    last_hovered_link: Option<u32>,

    config: Arc<Config>,
    cell_size: (f32, f32),

    pub last_mouse_pos: (f32, f32),
    decorations_dirty: bool,
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

impl Renderer {
    pub async fn new(window: Arc<Window>, config: Arc<Config>) -> Self {
        let gpu = GpuState::new(window.as_ref(), &config).await;
        let cache = Cache::new(&gpu.device);

        let cell_size = {
            let mut temp_db = fontdb::Database::new();
            temp_db.load_font_data(Vec::from(FONT_BYTES));
            let mut temp_font_system = FontSystem::new_with_locale_and_db("en-US".into(), temp_db);
            let mut temp_buffer = Buffer::new(
                &mut temp_font_system,
                Metrics::new(config.font_size, config.font_size),
            );
            temp_buffer.set_text(
                &mut temp_font_system,
                "W",
                &Attrs::new().family(Family::Monospace),
                Shaping::Advanced,
            );
            let cell_w = temp_buffer.layout_runs().next().unwrap().line_w;
            (cell_w, config.font_size)
        };

        let mut atlas = TextAtlas::new(&gpu.device, &gpu.queue, &cache, gpu.config.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &gpu.device, MultisampleState::default(), None);

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

        let bg_cache = LruCache::new(NonZeroUsize::new(15000).unwrap());
        let underline_cache = LruCache::new(NonZeroUsize::new(12000).unwrap());
        let undercurl_cache = LruCache::new(NonZeroUsize::new(12000).unwrap());

        let bg_clear_color = {
            let (r, g, b) = config.colors.background;
            let a = config.background_opacity;
            let srgb_to_linear_f64 = |c: u8| (c as f64 / 255.0).powf(2.2);
            wgpu::Color {
                r: srgb_to_linear_f64(r),
                g: srgb_to_linear_f64(g),
                b: srgb_to_linear_f64(b),
                a: a as f64,
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
            bg_cache,
            underline_cache,
            undercurl_cache,
            cache,
            atlas,
            text_renderer,
            last_scroll_offset: 0,
            last_selection: None,
            last_hovered_link: None,
            cell_size,
            config,
            last_mouse_pos: (0.0, 0.0),
            decorations_dirty: true,
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

    pub fn pixels_to_grid(&self, pos: (f32, f32), top_padding: f32) -> (usize, usize) {
        let (cell_w, cell_h) = self.cell_size;
        let col = (pos.0 / cell_w).floor() as usize;
        let row = ((pos.1 - top_padding) / cell_h).floor() as usize;
        let (grid_cols, _grid_rows) = self.grid_size(top_padding);

        (col.min(grid_cols.saturating_sub(1)), row)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
        hovered_link_id: Option<u32>,
        top_padding: f32,
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
        let globals = Globals {
            screen_size: [width as f32, height as f32],
            cell_size: [self.cell_size.0, self.cell_size.1],
            _padding: 0.0,
        };
        self.gpu
            .queue
            .write_buffer(&self.globals_buffer, 0, bytemuck::cast_slice(&[globals]));

        let needs_decoration_update = term.is_dirty
            || self.last_scroll_offset != term.scroll_offset
            || self.last_selection != selection
            || self.last_hovered_link != hovered_link_id
            || self.decorations_dirty;

        if needs_decoration_update {
            self.prepare_decorations(term, selection, hovered_link_id, top_padding);
            self.decorations_dirty = false;
        }

        let text_areas: Vec<TextArea> = (0..self.grid_size(top_padding).1)
            .filter_map(|y| {
                term.grid()
                    .get_display_row(y, term.scroll_offset)
                    .and_then(|row| {
                        if row.is_dirty {
                            None
                        } else {
                            row.render_cache.as_ref()
                        }
                    })
                    .map(|buffer| TextArea {
                        buffer,
                        left: 0.0,
                        top: (y as f32 * self.cell_size.1) + top_padding,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.surface_size().0 as i32,
                            bottom: self.surface_size().1 as i32,
                        },
                        custom_glyphs: &[],
                        default_color: glyphon::Color::rgb(0xFF, 0xFF, 0xFF),
                    })
            })
            .collect();

        {
            let Self {
                gpu,
                atlas,
                cache,
                text_renderer,
                bg,
                underline,
                undercurl,
                vertex_buffer,
                globals_bind_group,
                bg_clear_color,
                ..
            } = self;

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

            text_renderer.render(atlas, &viewport, &mut rpass).unwrap();
        }

        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();

        self.last_scroll_offset = term.scroll_offset;
        self.last_selection = selection;
        self.last_hovered_link = hovered_link_id;
    }

    /// Prepare background colors and all decorations
    fn prepare_decorations(
        &mut self,
        term: &mut TerminalState,
        selection: Option<((usize, usize), (usize, usize))>,
        hovered_link_id: Option<u32>,
        top_padding: f32,
    ) {
        let (_grid_cols, grid_rows) = self.grid_size(top_padding);
        let cursor_visible = term.cursor_visible && term.scroll_offset == 0;

        let default_bg_rgb = screen_grid::Rgb(
            self.config.colors.background.0,
            self.config.colors.background.1,
            self.config.colors.background.2,
        );

        // Clear old instance data
        self.bg.instances.clear();
        self.underline.instances.clear();
        self.undercurl.instances.clear();

        // Draw fake titlebar if needed
        #[cfg(target_os = "macos")]
        if top_padding > 0.0 {
            self.bg.instances.push(BgInstance {
                position: [0.0, 0.0],
                color: [0, 0, 0, 77],
            });
        }

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

                let y_pos = (y as f32 * self.cell_size.1) + top_padding;

                // Fast path
                if let Some(cached_bgs) = self.bg_cache.get(&row_hash) {
                    self.bg
                        .instances
                        .extend(cached_bgs.iter().map(|inst| BgInstance {
                            position: [inst.position[0], y_pos],
                            color: inst.color,
                        }));

                    if let Some(cached_underlines) = self.underline_cache.get(&row_hash) {
                        self.underline
                            .instances
                            .extend(cached_underlines.iter().map(|inst| UnderlineInstance {
                                position: [inst.position[0], y_pos],
                                color: inst.color,
                            }));
                    }

                    if let Some(cached_undercurls) = self.undercurl_cache.get(&row_hash) {
                        self.undercurl
                            .instances
                            .extend(cached_undercurls.iter().map(|inst| UndercurlInstance {
                                position: [inst.position[0], y_pos],
                                color: inst.color,
                            }));
                    }
                } else {
                    // Slow path
                    let mut row_bgs = Vec::new();
                    let mut row_underlines = Vec::new();
                    let mut row_undercurls = Vec::new();

                    for (x, cell) in grid_row.cells.iter().enumerate() {
                        let is_cursor =
                            cursor_visible && y == term.grid().cur_y && x == term.grid().cur_x;

                        let mut fg = cell.fg;
                        let mut bg = cell.bg;

                        if cell.flags.contains(CellFlags::INVERSE) {
                            std::mem::swap(&mut fg, &mut bg);
                        }

                        // Always draw the normal background color
                        let bg_color_rgb = bg;
                        if bg_color_rgb != default_bg_rgb {
                            row_bgs.push(BgInstance {
                                position: [x as f32 * self.cell_size.0, 0.0],
                                color: [bg_color_rgb.0, bg_color_rgb.1, bg_color_rgb.2, 255],
                            });
                        }

                        // If it's the cursor, draw another block
                        // on top, using the cursor color
                        if is_cursor {
                            let (r, g, b) = self.config.colors.cursor;
                            row_bgs.push(BgInstance {
                                position: [x as f32 * self.cell_size.0, 0.0],
                                color: [r, g, b, 255],
                            });
                        }

                        // Decorations
                        let decoration_fg = if is_cursor {
                            let (r, g, b) = self.config.colors.cursor_text;
                            Rgb(r, g, b)
                        } else {
                            fg
                        };
                        let final_fg_color =
                            [decoration_fg.0, decoration_fg.1, decoration_fg.2, 255];
                        let cell_x_pos = x as f32 * self.cell_size.0;

                        if cell.flags.contains(CellFlags::UNDERLINE) {
                            row_underlines.push(UnderlineInstance {
                                position: [cell_x_pos, 0.0],
                                color: final_fg_color,
                            });
                        }

                        let is_hovered_link =
                            cell.link_id == hovered_link_id && hovered_link_id.is_some();
                        if cell.flags.contains(CellFlags::UNDERCURL) || is_hovered_link {
                            row_undercurls.push(UndercurlInstance {
                                position: [cell_x_pos, 0.0],
                                color: final_fg_color,
                            });
                        }
                    }

                    self.bg
                        .instances
                        .extend(row_bgs.iter().map(|inst| BgInstance {
                            position: [inst.position[0], y_pos],
                            color: inst.color,
                        }));
                    self.underline
                        .instances
                        .extend(row_underlines.iter().map(|inst| UnderlineInstance {
                            position: [inst.position[0], y_pos],
                            color: inst.color,
                        }));
                    self.undercurl
                        .instances
                        .extend(row_undercurls.iter().map(|inst| UndercurlInstance {
                            position: [inst.position[0], y_pos],
                            color: inst.color,
                        }));

                    self.bg_cache.put(row_hash, row_bgs);
                    self.underline_cache.put(row_hash, row_underlines);
                    self.undercurl_cache.put(row_hash, row_undercurls);
                }
            }
        }

        let selection_bg_instances = self.prepare_selection_bg(selection, term, top_padding);
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
        top_padding: f32,
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
                        position: [
                            x as f32 * cell_size.0,
                            (y as f32 * cell_size.1) + top_padding,
                        ],
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
    pub fn grid_size(&self, top_padding: f32) -> (usize, usize) {
        let (w_px, h_px) = self.surface_size();
        let (cell_w, cell_h) = self.cell_size();

        let available_height = h_px as f32 - top_padding;

        (
            (w_px / cell_w) as usize,
            (available_height / cell_h as f32) as usize,
        )
    }
}

impl GpuState {
    async fn new(window: &Window, _config: &Config) -> Self {
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

        let alpha_mode = if caps.alpha_modes.contains(&CompositeAlphaMode::Inherit) {
            CompositeAlphaMode::Inherit
        } else {
            // Fallback if we can't use inherit (like on macOS)
            CompositeAlphaMode::PostMultiplied
        };

        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode: PresentMode::Fifo,
            alpha_mode,
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
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
