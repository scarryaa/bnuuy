use crate::Config;
use crate::shaper::Shaper;
use arboard::Clipboard;
use crossbeam_channel::{Receiver, unbounded};
use glyphon::{FontSystem, SwashCache, fontdb};
use portable_pty::PtySize;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{sync::Arc, thread};
use winit::event::MouseScrollDelta;
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;

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

pub struct App {
    renderer: Option<Renderer>,
    term: Option<Arc<Mutex<TerminalState>>>,
    pty: Option<PtyHandles>,
    reader: Option<JoinHandle<()>>,
    modifiers: ModifiersState,
    pty_data_receiver: Option<Receiver<Vec<u8>>>,
    proxy: Option<EventLoopProxy<CustomEvent>>,
    clipboard: Option<Clipboard>,
    selection_start: Option<(usize, usize)>, // (col, row)
    selection_end: Option<(usize, usize)>,   // (col, row)
    is_mouse_dragging: bool,
    hovered_link_id: Option<u32>,

    font_system: Option<FontSystem>,
    swash_cache: Option<SwashCache>,
    fallback_cache: Option<HashMap<char, bool>>,
    pty_data_buffer: VecDeque<u8>,
    config: Arc<Config>,

    #[cfg(target_os = "macos")]
    top_padding: f32,
}

impl App {
    pub fn new(proxy: EventLoopProxy<CustomEvent>, config: Arc<Config>) -> Self {
        Self {
            proxy: Some(proxy),
            clipboard: Clipboard::new().ok(),
            is_mouse_dragging: false,
            hovered_link_id: None,
            renderer: None,
            term: None,
            pty: None,
            reader: None,
            modifiers: ModifiersState::default(),
            pty_data_receiver: None,
            selection_start: None,
            selection_end: None,
            font_system: None,
            swash_cache: None,
            fallback_cache: None,
            pty_data_buffer: VecDeque::with_capacity(1024 * 1024), // 1MB capacity
            config,

            #[cfg(target_os = "macos")]
            top_padding: 0.0,
        }
    }

    fn get_selected_text(&self) -> Option<String> {
        let (start_pos, end_pos) = match (self.selection_start, self.selection_end) {
            (Some(start), Some(end)) => (start, end),
            _ => return None,
        };

        let term_lock = self.term.as_ref()?.lock().ok()?;

        let (start, end) =
            if start_pos.1 < end_pos.1 || (start_pos.1 == end_pos.1 && start_pos.0 <= end_pos.0) {
                (start_pos, end_pos)
            } else {
                (end_pos, start_pos)
            };

        let (start_col, start_row) = start;
        let (end_col, end_row) = end;

        let mut result = String::new();

        for y in start_row..=end_row {
            // Add a newline for every line after the first one in the selection
            if y > start_row {
                result.push('\n');
            }

            if let Some(row) = term_lock.grid().get_display_row(y, term_lock.scroll_offset) {
                let line_start = if y == start_row { start_col } else { 0 };
                let line_end = if y == end_row {
                    end_col
                } else {
                    term_lock.grid().cols
                };

                let line_text: String = row
                    .cells
                    .iter()
                    .skip(line_start)
                    .take(line_end.saturating_sub(line_start))
                    .map(|cell| cell.ch)
                    .collect();

                // For multi-line selections, trim trailing whitespace from all but the last line
                if y < end_row {
                    result.push_str(line_text.trim_end());
                } else {
                    result.push_str(&line_text);
                }
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}

impl ApplicationHandler<CustomEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.renderer.is_none() {
            let mut main_db = fontdb::Database::new();

            main_db.load_font_data(Vec::from(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/fonts/HackNerdFontMono-Regular.ttf"
            ))));
            main_db.load_font_data(Vec::from(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/fonts/HackNerdFontMono-Italic.ttf"
            ))));
            main_db.load_font_data(Vec::from(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/fonts/HackNerdFontMono-Bold.ttf"
            ))));
            main_db.load_font_data(Vec::from(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/fonts/HackNerdFontMono-BoldItalic.ttf"
            ))));

            main_db.load_font_data(Vec::from(include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../assets/fonts/DejaVuSansMono.ttf"
            ))));

            main_db.set_monospace_family("Hack Nerd Font Mono");
            self.font_system = Some(FontSystem::new_with_locale_and_db("en-US".into(), main_db));

            self.swash_cache = Some(SwashCache::new());
            self.fallback_cache = Some(HashMap::new());

            let mut window_attributes =
                WindowAttributes::default().with_transparent(self.config.background_opacity < 1.0);

            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::WindowAttributesExtMacOS;
                if self.config.macos_transparent_titlebar {
                    window_attributes = window_attributes
                        .with_titlebar_transparent(true)
                        .with_fullsize_content_view(true);
                }
            }

            let window = Arc::new(el.create_window(window_attributes).unwrap());

            #[cfg(target_os = "macos")]
            if self.config.macos_transparent_titlebar {
                self.top_padding = 28.0;
            }

            let ren = pollster::block_on(Renderer::new(window.clone(), self.config.clone()));

            let (cols, rows) = ren.grid_size(
                #[cfg(target_os = "macos")]
                self.top_padding,
                #[cfg(not(target_os = "macos"))]
                0.0,
            );

            let term = Arc::new(Mutex::new(TerminalState::new(
                cols,
                rows,
                self.config.clone(),
            )));

            let pty = spawn_shell(cols as u16, rows as u16, self.config.clone());

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
            ren.window.set_cursor(winit::window::CursorIcon::Text);

            self.renderer = Some(ren);
            self.term = Some(term);
            self.pty = Some(pty);
            self.reader = Some(handle);
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: CustomEvent) {
        match event {
            CustomEvent::PtyData => {
                if let Some(rx) = &self.pty_data_receiver {
                    // Drain the entire crossbeam channel into the internal pty_data_buffer
                    for data in rx.try_iter() {
                        self.pty_data_buffer.extend(data);
                    }
                }

                // Request a single redraw to start the work loop
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

                    let (cols, rows) = renderer.grid_size(
                        #[cfg(target_os = "macos")]
                        self.top_padding,
                        #[cfg(not(target_os = "macos"))]
                        0.0,
                    );

                    if let Some(term_arc) = &self.term {
                        if let Ok(mut t) = term_arc.lock() {
                            t.normal_grid.resize(cols, rows);
                            t.alternate_grid.resize(cols, rows);
                            t.is_dirty = true;
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
                    if let (
                        Some(renderer),
                        Some(term_arc),
                        Some(font_system),
                        Some(swash_cache),
                        Some(fallback_cache),
                    ) = (
                        &mut self.renderer,
                        &self.term,
                        &mut self.font_system,
                        &mut self.swash_cache,
                        &mut self.fallback_cache,
                    ) {
                        let frame_start_time = Instant::now();
                        let processing_budget = Duration::from_millis(12);

                        let more_shaping_work: bool;
                        {
                            let mut term = term_arc.lock().unwrap();

                            // Empty the reservoir as fast as possible
                            if !self.pty_data_buffer.is_empty() {
                                while frame_start_time.elapsed() < processing_budget {
                                    if self.pty_data_buffer.is_empty() {
                                        break;
                                    }

                                    const PARSE_CHUNK_SIZE: usize = 1024 * 64; // 64KiB
                                    let to_process =
                                        self.pty_data_buffer.len().min(PARSE_CHUNK_SIZE);
                                    let data_chunk: Vec<u8> =
                                        self.pty_data_buffer.drain(..to_process).collect();
                                    term.feed(&data_chunk);
                                }
                            }

                            // After parsing, we shape a fixed number of lines
                            let mut shaper = Shaper::new(self.config.clone());
                            more_shaping_work =
                                shaper.shape_budgeted(font_system, fallback_cache, &mut term, 400);
                        }

                        let mut term_lock = term_arc.lock().unwrap();
                        let selection = if let (Some(start), Some(end)) =
                            (self.selection_start, self.selection_end)
                        {
                            Some((start, end))
                        } else {
                            None
                        };

                        renderer.render(
                            font_system,
                            swash_cache,
                            &mut term_lock,
                            selection,
                            self.hovered_link_id,
                            #[cfg(target_os = "macos")]
                            self.top_padding,
                            #[cfg(not(target_os = "macos"))]
                            0.0,
                        );

                        if !self.pty_data_buffer.is_empty() || more_shaping_work {
                            renderer.window.request_redraw();
                        }
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if button == winit::event::MouseButton::Left {
                        if state == winit::event::ElementState::Pressed {
                            #[cfg(target_os = "macos")]
                            let is_link_modifier_pressed = self.modifiers.super_key();
                            #[cfg(not(target_os = "macos"))]
                            let is_link_modifier_pressed = self.modifiers.control_key();

                            if is_link_modifier_pressed {
                                let (col, row) = renderer.pixels_to_grid(
                                    renderer.last_mouse_pos,
                                    #[cfg(target_os = "macos")]
                                    self.top_padding,
                                    #[cfg(not(target_os = "macos"))]
                                    0.0,
                                );
                                if let Some(term_arc) = &self.term {
                                    if let Ok(term) = term_arc.lock() {
                                        if let Some(link_id) = term.get_link_at(col, row) {
                                            if let Some(url) = term.links.get(&link_id) {
                                                opener::open(url).ok();
                                                return;
                                            }
                                        }
                                    }
                                }
                            }

                            self.is_mouse_dragging = true;

                            self.selection_start = Some(renderer.pixels_to_grid(
                                renderer.last_mouse_pos,
                                #[cfg(target_os = "macos")]
                                self.top_padding,
                                #[cfg(not(target_os = "macos"))]
                                0.0,
                            ));
                            self.selection_end = self.selection_start;

                            if let Some(term_arc) = &self.term {
                                term_arc.lock().unwrap().is_dirty = true;
                            }
                            renderer.window.request_redraw();
                        } else {
                            self.is_mouse_dragging = false;

                            if let Some(text) = self.get_selected_text() {
                                if let Some(clipboard) = &mut self.clipboard {
                                    clipboard.set_text(text).ok();
                                }
                            }
                        }
                    } else if button == winit::event::MouseButton::Left
                        && state == winit::event::ElementState::Pressed
                        && self.modifiers.control_key()
                    {
                        let (col, row) = renderer.pixels_to_grid(
                            renderer.last_mouse_pos,
                            #[cfg(target_os = "macos")]
                            self.top_padding,
                            #[cfg(not(target_os = "macos"))]
                            0.0,
                        );
                        if let Some(term_arc) = &self.term {
                            if let Ok(term) = term_arc.lock() {
                                if let Some(link_id) = term.get_link_at(col, row) {
                                    if let Some(url) = term.links.get(&link_id) {
                                        opener::open(url).ok();
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                WindowEvent::CursorEntered { .. } => {
                    renderer.window.set_cursor(winit::window::CursorIcon::Text);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    renderer.last_mouse_pos = (position.x as f32, position.y as f32);
                    update_hover_state(
                        &self.term,
                        &mut self.hovered_link_id,
                        renderer,
                        #[cfg(target_os = "macos")]
                        self.top_padding,
                        #[cfg(not(target_os = "macos"))]
                        0.0,
                    );

                    if self.is_mouse_dragging {
                        self.selection_end = Some(renderer.pixels_to_grid(
                            renderer.last_mouse_pos,
                            #[cfg(target_os = "macos")]
                            self.top_padding,
                            #[cfg(not(target_os = "macos"))]
                            0.0,
                        ));

                        if let Some(term_arc) = &self.term {
                            term_arc.lock().unwrap().is_dirty = true;
                        }

                        renderer.window.request_redraw();
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

                            if let Some(renderer) = &self.renderer {
                                renderer.window.request_redraw();
                            }
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    use std::io::Write;
                    use winit::keyboard::{Key, KeyCode, PhysicalKey};

                    if event.state == winit::event::ElementState::Pressed {
                        let mut text_to_send: Option<String> = None;

                        #[cfg(target_os = "macos")]
                        let is_shortcut_modifier = self.modifiers.super_key();

                        #[cfg(not(target_os = "macos"))]
                        let is_shortcut_modifier =
                            self.modifiers.control_key() && self.modifiers.shift_key();

                        // Check for shortcut modifier
                        if is_shortcut_modifier {
                            if let PhysicalKey::Code(key_code) = event.physical_key {
                                match key_code {
                                    KeyCode::KeyC => {
                                        if let Some(text) = self.get_selected_text() {
                                            if let Some(clipboard) = &mut self.clipboard {
                                                clipboard.set_text(text).ok();
                                            }
                                        }

                                        return;
                                    }
                                    KeyCode::KeyV => {
                                        if let Some(clipboard) = &mut self.clipboard {
                                            if let Ok(text) = clipboard.get_text() {
                                                text_to_send = Some(text);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        // Handle Ctrl by itself
                        else if self.modifiers.control_key() {
                            if let Key::Character(s) = &event.logical_key {
                                let s_lower = s.to_lowercase();
                                if let Some(ch) = s_lower.chars().next() {
                                    if ('a'..='z').contains(&ch) {
                                        let ctrl_code = (ch as u8 - b'a' + 1) as char;
                                        text_to_send = Some(ctrl_code.to_string());
                                    }
                                }
                            }
                        }

                        // If no modifier combo, check for other special keys
                        if text_to_send.is_none() {
                            if let PhysicalKey::Code(key_code) = event.physical_key {
                                let special_text = match key_code {
                                    KeyCode::Enter => "\r",
                                    KeyCode::Backspace => "\x7F",
                                    KeyCode::Escape => "\x1b",
                                    KeyCode::Tab => {
                                        if self.modifiers.shift_key() {
                                            // If shift is held, send the "back-tab" escape sequence
                                            "\x1b[Z"
                                        } else {
                                            // Otherwise, send a normal tab
                                            "\t"
                                        }
                                    }
                                    KeyCode::ArrowUp => "\x1b[A",
                                    KeyCode::ArrowDown => "\x1b[B",
                                    KeyCode::ArrowRight => "\x1b[C",
                                    KeyCode::ArrowLeft => "\x1b[D",
                                    _ => "", // Unhandled special key
                                };
                                if !special_text.is_empty() {
                                    text_to_send = Some(special_text.to_string());
                                }
                            }
                        }

                        // If still nothing, fall back to the text event from winit
                        if text_to_send.is_none() {
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

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        println!("Exiting app");

        if let Some(pty) = &mut self.pty {
            pty.child.kill().ok();
        }

        self.pty = None;

        if let Some(reader) = self.reader.take() {
            reader.join().ok();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // Might lose GPU state here -- TODO?
    }

    fn memory_warning(&mut self, _event_loop: &ActiveEventLoop) {
        // TODO clear cache if needed?
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        // TODO utilize this if needed
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Check if reader thread has finished
        if let Some(handle) = &self.reader {
            if handle.is_finished() {
                println!("PTY reader thread finished. Exiting");

                if let Some(h) = self.reader.take() {
                    let _ = h.join();
                }

                event_loop.exit();
            }
        }
    }
}

fn update_hover_state(
    term: &Option<Arc<Mutex<TerminalState>>>,
    hovered_link_id: &mut Option<u32>,
    renderer: &Renderer,
    top_padding: f32,
) {
    let (col, row) = renderer.pixels_to_grid(renderer.last_mouse_pos, top_padding);
    let new_hovered_id = term
        .as_ref()
        .and_then(|term_arc| term_arc.lock().ok())
        .and_then(|term| term.get_link_at(col, row));

    let current_cursor = if new_hovered_id.is_some() {
        winit::window::CursorIcon::Pointer
    } else {
        winit::window::CursorIcon::Text
    };
    renderer.window.set_cursor(current_cursor);

    if new_hovered_id != *hovered_link_id {
        *hovered_link_id = new_hovered_id;
        renderer.window.request_redraw();
    }
}
