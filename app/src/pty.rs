use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub struct PtyHandles {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send>,
}

pub fn spawn_shell(cols: u16, rows: u16) -> Box<dyn Child + Send> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty failed");

    let shell = if cfg!(windows) { "powershell" } else { "bash" };
    let cmd = CommandBuilder::new(shell);

    let child = pair.slave.spawn_command(cmd).expect("spawn_command failed");

    drop(pair.slave);
    child
}
