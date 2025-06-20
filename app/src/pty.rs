use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{io::Write, sync::Arc};

use crate::config::Config;

pub struct PtyHandles {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn Child + Send>,
}

pub fn spawn_shell(cols: u16, rows: u16, config: Arc<Config>) -> PtyHandles {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty failed");

    let mut cmd = CommandBuilder::new(&config.shell[0]);
    cmd.args(&config.shell[1..]);

    cmd.env("TERM", "xterm-256color");

    let child = pair.slave.spawn_command(cmd).expect("spawn failed");
    let writer = pair.master.take_writer().expect("writer");

    PtyHandles {
        master: pair.master,
        writer,
        child,
    }
}
