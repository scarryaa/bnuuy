mod app;
mod renderer;

use crate::app::App;
use std::error::Error;
use winit::event_loop::EventLoop;

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App::default();
    event_loop.run_app(&mut app)?;

    Ok(())
}
