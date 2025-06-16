use portable_pty::PtySize;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::{sync::Arc, thread};

use crate::{
    pty::{PtyHandles, spawn_shell},
    renderer::Renderer,
    terminal::TerminalState,
};
use winit::{
    application::ApplicationHandler, event::WindowEvent, event_loop::ActiveEventLoop,
    window::WindowAttributes,
};

#[derive(Default)]
pub struct App {
    renderer: Option<Renderer>,
    term: Option<Arc<Mutex<TerminalState>>>,
    pty: Option<PtyHandles>,
    reader: Option<JoinHandle<()>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.renderer.is_none() {
            let window = Arc::new(el.create_window(WindowAttributes::default()).unwrap());
            let ren = pollster::block_on(Renderer::new(window.clone()));

            let (cols, rows) = ren.grid_size();

            let term = Arc::new(Mutex::new(TerminalState::new(cols, rows)));

            let pty = spawn_shell(cols as u16, rows as u16);
            let reader = pty.master.try_clone_reader().expect("clone reader");

            let term_for_thread = Arc::clone(&term);
            let window_for_thread = Arc::clone(&window);

            let handle = thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];

                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut t) = term_for_thread.lock() {
                                t.feed(&buf[..n]);
                            }
                            window_for_thread.request_redraw();
                        }
                    }
                }
            });

            self.renderer = Some(ren);
            self.term = Some(term);
            self.pty = Some(pty);
            self.reader = Some(handle);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        if let Some(renderer) = &mut self.renderer {
            if renderer.window_id() != window_id {
                return;
            }

            match event {
                WindowEvent::CloseRequested => {
                    println!("Window close requested. Exiting");
                    event_loop.exit();
                }
                WindowEvent::Resized(new_size) => {
                    renderer.resize(new_size.width, new_size.height);

                    let (cell_w, cell_h) = renderer.cell_size();
                    let cols = (new_size.width / cell_w) as usize;
                    let rows = (new_size.height / cell_h) as usize;

                    if let Some(term_arc) = &self.term {
                        if let Ok(mut t) = term_arc.lock() {
                            t.grid.resize(cols, rows);
                        }
                    }

                    if let Some(pty) = &self.pty {
                        let _ = pty.master.resize(PtySize {
                            cols: cols as u16,
                            rows: rows as u16,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let (Some(renderer), Some(term_arc)) = (&mut self.renderer, &self.term) {
                        if let Ok(mut term) = term_arc.lock() {
                            renderer.render(&mut term);
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    use std::io::Write;
                    use winit::keyboard::{KeyCode, PhysicalKey};

                    if event.state == winit::event::ElementState::Pressed {
                        let mut text_to_send: Option<String> = None;

                        // Handle special keys first
                        if let PhysicalKey::Code(key_code) = event.physical_key {
                            text_to_send = Some(match key_code {
                                KeyCode::Enter => "\r".into(),
                                KeyCode::Backspace => "\x08".into(),
                                KeyCode::Escape => "\x1b".into(),
                                KeyCode::Tab => "\t".into(),
                                KeyCode::ArrowUp => "\x1b[A".into(),
                                KeyCode::ArrowDown => "\x1b[B".into(),
                                KeyCode::ArrowRight => "\x1b[C".into(),
                                KeyCode::ArrowLeft => "\x1b[D".into(),
                                // TODO add more keys
                                _ => "".into(),
                            });
                        }

                        if text_to_send.as_deref() == Some("") || text_to_send.is_none() {
                            text_to_send = event.text.map(|t| t.to_string());
                        }

                        // Send result to PTY
                        if let Some(text) = text_to_send {
                            if !text.is_empty() {
                                if let Some(pty) = &mut self.pty {
                                    let _ = pty.writer.write_all(text.as_bytes());
                                }
                            }
                        }
                    }
                }
                _ => (),
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Check if reader thread has finished
        if let Some(handle) = &self.reader {
            if handle.is_finished() {
                if let Some(h) = self.reader.take() {
                    let _ = h.join();
                }

                event_loop.exit();
            }
        }
    }
}
