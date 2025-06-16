use screen_grid::{CellFlags, Rgb, ScreenGrid};
use vte::Parser;

#[derive(Clone, Copy)]
struct Attrs {
    fg: Rgb,
    bg: Rgb,
    flags: CellFlags,
}

impl Default for Attrs {
    fn default() -> Self {
        Self {
            fg: Rgb(0xC0, 0xC0, 0xC0),
            bg: Rgb(0x00, 0x00, 0x00),
            flags: CellFlags::empty(),
        }
    }
}

struct GridPerform<'a> {
    grid: &'a mut ScreenGrid,
    attr: Attrs,
}

impl<'a> vte::Perform for GridPerform<'a> {
    fn print(&mut self, c: char) {
        self.grid
            .put_char_ex(c, self.attr.fg, self.attr.bg, self.attr.flags);
    }

    fn execute(&mut self, byte: u8) {
        if byte == b'\n' {
            self.grid.line_feed();
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        final_byte: char,
    ) {
        if final_byte != 'm' {
            return;
        } // only SGR for now

        if params.is_empty() {
            // “CSI m” → reset
            self.attr = Attrs::default();
            return;
        }

        for p in params.iter() {
            let n = p[0] as u8;

            match n {
                0 => self.attr = Attrs::default(),
                1 => self.attr.flags.insert(CellFlags::BOLD),
                2 => self.attr.flags.insert(CellFlags::FAINT),
                22 => self.attr.flags.remove(CellFlags::BOLD | CellFlags::FAINT),

                30..=37 => self.attr.fg = ansi_16(n - 30, false),
                90..=97 => self.attr.fg = ansi_16(n - 90, true),
                39 => self.attr.fg = Attrs::default().fg,

                40..=47 => self.attr.bg = ansi_16(n - 40, false),
                100..=107 => self.attr.bg = ansi_16(n - 100, true),
                49 => self.attr.bg = Attrs::default().bg,
                _ => {}
            }
        }
    }
}

fn ansi_16(idx: u8, bright: bool) -> Rgb {
    const BASE: [(u8, u8, u8); 8] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
    ];
    let (r, g, b) = BASE[idx as usize];
    if bright {
        Rgb(
            r.saturating_add(50u8),
            g.saturating_add(50u8),
            b.saturating_add(50u8),
        )
    } else {
        Rgb(r, g, b)
    }
}

pub struct TerminalState {
    pub grid: ScreenGrid,
    parser: Parser,
}

impl TerminalState {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            grid: ScreenGrid::new(cols, rows, 10_000),
            parser: Parser::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut performer = GridPerform {
            grid: &mut self.grid,
            attr: Attrs::default(),
        };
        self.parser.advance(&mut performer, bytes);
    }
}
