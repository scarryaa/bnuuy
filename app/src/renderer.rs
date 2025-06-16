use crate::terminal::TerminalState;
use glam::Vec2;
use screen_grid::CellFlags;
use std::sync::Arc;
use wgpu::{util::StagingBelt, *};
use wgpu_glyph::{GlyphBrush, GlyphBrushBuilder, Section, Text, ab_glyph::FontArc};
use winit::window::{Window, WindowId};

/// Compile-time embedded font
const FONT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/fonts/DejaVuSansMono.ttf"
));

/// Monospace cell metrics (px)
const CELL_W: f32 = 9.0;
const CELL_H: f32 = 16.0;
const STAGING_CHUNK: usize = 1 << 16;

pub struct Renderer {
    window: Arc<Window>,
    gpu: GpuState,
    text: TextRenderer,
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

        Self { window, gpu, text }
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

    pub fn render(&mut self, term: &TerminalState) {
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

        self.queue_glyphs(term);

        {
            let _rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
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
        }

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

    fn queue_glyphs(&mut self, term: &TerminalState) {
        let TextRenderer { brush, cell, .. } = &mut self.text;

        for y in 0..term.grid.rows {
            if let Some(row) = term.grid.visible_row(y) {
                for (x, cell_data) in row.iter().enumerate() {
                    let ch = cell_data.ch;
                    if ch == ' ' {
                        continue;
                    } // skip blanks

                    let px = x as f32 * cell.x;
                    let py = y as f32 * cell.y;

                    let mut buf = [0u8; 4];
                    let glyph = ch.encode_utf8(&mut buf);

                    let mut rgba = [
                        cell_data.fg.0 as f32 / 255.0,
                        cell_data.fg.1 as f32 / 255.0,
                        cell_data.fg.2 as f32 / 255.0,
                        1.0,
                    ];

                    if cell_data.flags.contains(CellFlags::FAINT) {
                        // 50 % intensity
                        for chan in &mut rgba[0..3] {
                            *chan *= 0.5;
                        }
                    }

                    brush.queue(Section {
                        screen_position: (px, py),
                        text: vec![Text::new(glyph).with_color(rgba)],
                        ..Section::default()
                    });
                }
            }
        }
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
        let brush = GlyphBrushBuilder::using_font(font).build(device, format);

        Self {
            brush,
            staging_belt: StagingBelt::new(STAGING_CHUNK.try_into().unwrap()),
            cell: Vec2::new(CELL_W, CELL_H),
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
