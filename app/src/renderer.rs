use crate::terminal::TerminalState;
use std::sync::Arc;
use wgpu::util::StagingBelt;
use wgpu::*;
use wgpu_glyph::Section;
use wgpu_glyph::Text;
use wgpu_glyph::ab_glyph::FontArc;
use wgpu_glyph::{GlyphBrush, GlyphBrushBuilder};
use winit::{window::Window, window::WindowId};

pub struct Renderer {
    pub window: Arc<Window>,
    pub surface: Surface<'static>,
    pub device: Device,
    pub queue: Queue,
    pub staging_belt: StagingBelt,
    pub config: SurfaceConfiguration,
    pub glyph_brush: GlyphBrush<()>,
    pub font_w: f32,
    pub font_h: f32,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        let instance = Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No suitable adapter");

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .expect("Unable to create device");

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

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

        const FONT_BYTES: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/fonts/DejaVuSansMono.ttf"
        ));

        let font = FontArc::try_from_slice(FONT_BYTES).unwrap();
        let glyph_brush = GlyphBrushBuilder::using_font(font).build(&device, format);

        // TODO refine this
        let font_h = 16.0;
        let font_w = 9.0;
        let staging_belt = StagingBelt::new(1 << 16);

        Self {
            window,
            surface,
            device,
            queue,
            staging_belt,
            config,
            glyph_brush,
            font_w,
            font_h,
        }
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn render(&mut self, term: &mut TerminalState) {
        self.staging_belt.recall();

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.resize(self.config.width, self.config.height);
                return;
            }
            Err(SurfaceError::OutOfMemory) => panic!("Out of memory"),
            Err(e) => {
                eprintln!("Surface error: {e:?}");
                return;
            }
        };

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("terminal-encoder"),
            });

        // Queue glyphs for dirty cells
        for y in 0..term.grid.rows {
            let row = term.grid.visible_row(y).unwrap();
            for (x, cell) in row.iter().enumerate() {
                let px = x as f32 * self.font_w;
                let py = y as f32 * self.font_h;

                self.glyph_brush.queue(Section {
                    screen_position: (px, py),
                    text: vec![Text::new(&cell.ch.to_string()).with_color(
                        [cell.fg.0, cell.fg.1, cell.fg.2, 255].map(|c| c as f32 / 255.0),
                    )],
                    ..Section::default()
                });
            }
        }

        // Clear whole frame for now
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

        self.glyph_brush
            .draw_queued(
                &self.device,
                &mut self.staging_belt,
                &mut encoder,
                &view,
                self.config.width,
                self.config.height,
            )
            .expect("draw_queued");

        self.staging_belt.finish();
        self.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    pub fn cell_size(&self) -> (u32, u32) {
        (self.font_w as u32, self.font_h as u32)
    }
}
