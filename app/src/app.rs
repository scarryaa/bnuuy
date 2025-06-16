use crossbeam_channel::{Receiver, unbounded};
use portable_pty::PtySize;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::{sync::Arc, thread};
use winit::event::MouseScrollDelta;
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, ModifiersState};

use crate::{
    pty::{PtyHandles, spawn_shell},
    renderer::Renderer,
    terminal::TerminalState,
};
use winit::{
    application::ApplicationHandler, event::WindowEvent, event_loop::ActiveEventLoop,
    window::WindowAttributes,
};

#[derive(Debug, Clone, Copy)]
pub enum CustomEvent {
    PtyData,
}

#[derive(Default)]
pub struct App {
    renderer: Option<Renderer>,
    term: Option<Arc<Mutex<TerminalState>>>,
    pty: Option<PtyHandles>,
    reader: Option<JoinHandle<()>>,
    modifiers: ModifiersState,
    pty_data_receiver: Option<Receiver<Vec<u8>>>,
    proxy: Option<EventLoopProxy<CustomEvent>>,
}

impl App {
    pub fn new(proxy: EventLoopProxy<CustomEvent>) -> Self {
        Self {
            proxy: Some(proxy),
            ..Default::default()
        }
    }
}

impl ApplicationHandler<CustomEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.renderer.is_none() {
            let window = Arc::new(el.create_window(WindowAttributes::default()).unwrap());
            let ren = pollster::block_on(Renderer::new(window.clone()));

            let (cols, rows) = ren.grid_size();

            let term = Arc::new(Mutex::new(TerminalState::new(cols, rows)));

            let pty = spawn_shell(cols as u16, rows as u16);

            // Create a channel
            let (tx, rx) = unbounded();
            self.pty_data_receiver = Some(rx);

            let proxy = self.proxy.as_ref().unwrap().clone();
            let reader = pty.master.try_clone_reader().expect("clone reader");

            let handle = thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];

                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let data = buf[..n].to_vec();
                            if tx.send(data).is_err() {
                                break;
                            }

                            proxy.send_event(CustomEvent::PtyData).ok();
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: CustomEvent) {
        match event {
            CustomEvent::PtyData => {
                if let Some(renderer) = &self.renderer {
                    renderer.window.request_redraw();
                }
            }
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
                WindowEvent::ModifiersChanged(new_modifiers) => {
                    self.modifiers = new_modifiers.state();
                }
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
                    // Drain all pending data from the PTY channel and feed it to the terminal
                    if let (Some(term_arc), Some(rx)) = (&self.term, &self.pty_data_receiver) {
                        let mut term_lock = term_arc.lock().unwrap();
                        for data in rx.try_iter() {
                            term_lock.feed(&data);
                        }
                    }

                    // Render with the fully updated terminal state
                    if let (Some(renderer), Some(term_arc)) = (&mut self.renderer, &self.term) {
                        if let Ok(mut term) = term_arc.lock() {
                            renderer.render(&mut term);
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    if let Some(term_arc) = &self.term {
                        if let Ok(mut term) = term_arc.lock() {
                            let scroll_lines = match delta {
                                MouseScrollDelta::LineDelta(_, y) => y as i32,
                                MouseScrollDelta::PixelDelta(pos) => (pos.y / 16.0) as i32,
                            };

                            term.scroll_viewport(-scroll_lines);
                            renderer.window.request_redraw();
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    use std::io::Write;
                    use winit::keyboard::PhysicalKey;

                    if event.state == winit::event::ElementState::Pressed {
                        let mut text_to_send: Option<String> = None;

                        // Check for Ctrl + key combinations
                        if self.modifiers.control_key() {
                            if let Key::Character(s) = &event.logical_key {
                                // For keys a-z, generate control codes \x01 through \x1A
                                let s_lower = s.to_lowercase();
                                if let Some(ch) = s_lower.chars().next() {
                                    if ('a'..='z').contains(&ch) {
                                        let ctrl_code = (ch as u8 - b'a' + 1) as char;
                                        text_to_send = Some(ctrl_code.to_string());
                                    }
                                }
                            }
                        }

                        // If no Ctrl combo, check for other special keys
                        if text_to_send.is_none() {
                            if let PhysicalKey::Code(key_code) = event.physical_key {
                                text_to_send = Some(match key_code {
                                    winit::keyboard::KeyCode::Enter => "\r".into(),
                                    winit::keyboard::KeyCode::Backspace => "\x08".into(),
                                    winit::keyboard::KeyCode::Escape => "\x1b".into(),
                                    winit::keyboard::KeyCode::Tab => "\t".into(),
                                    winit::keyboard::KeyCode::ArrowUp => "\x1b[A".into(),
                                    winit::keyboard::KeyCode::ArrowDown => "\x1b[B".into(),
                                    winit::keyboard::KeyCode::ArrowRight => "\x1b[C".into(),
                                    winit::keyboard::KeyCode::ArrowLeft => "\x1b[D".into(),
                                    _ => "".into(), // Unhandled special key
                                });
                            }
                        }

                        // If still nothing, fall back to the text event
                        let is_unhandled_special = text_to_send.as_deref() == Some("");
                        if text_to_send.is_none() || is_unhandled_special {
                            text_to_send = event.text.map(|t| t.to_string());
                        }

                        // Send the final result to the PTY
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
