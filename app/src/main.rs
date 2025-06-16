mod app;
mod pty;
mod renderer;
mod terminal;

use crate::app::{App, CustomEvent};
use std::error::Error;
use winit::event_loop::EventLoop;

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<CustomEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    let mut app = App::new(proxy);

    event_loop.run_app(&mut app)?;

    Ok(())
}
