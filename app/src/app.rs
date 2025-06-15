use std::sync::Arc;

use crate::renderer::Renderer;
use winit::{
    application::ApplicationHandler, event::WindowEvent, event_loop::ActiveEventLoop,
    window::WindowAttributes,
};

#[derive(Default)]
pub struct App {
    renderer: Option<Renderer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.renderer.is_none() {
            let window = Arc::new(el.create_window(WindowAttributes::default()).unwrap());
            let renderer = pollster::block_on(Renderer::new(window));
            self.renderer = Some(renderer);
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
                }
                WindowEvent::RedrawRequested => {
                    renderer.render();
                }
                _ => (),
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(renderer) = &self.renderer {
            renderer.window.request_redraw();
        }
    }
}
