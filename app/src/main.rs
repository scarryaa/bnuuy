mod app;
mod config;
mod pty;
mod renderer;
mod shaper;
mod terminal;

use crate::{
    app::{App, CustomEvent},
    config::Config,
};
use std::{error::Error, sync::Arc};
use winit::event_loop::EventLoop;

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    // Load config
    let config = Config::load()?;
    let config = Arc::new(config);

    let event_loop = EventLoop::<CustomEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    let mut app = App::new(proxy, config);

    event_loop.run_app(&mut app)?;

    Ok(())
}
