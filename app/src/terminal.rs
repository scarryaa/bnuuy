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

struct VtePerformer<'a> {
    grid: &'a mut ScreenGrid,
    attrs: &'a mut Attrs,
}

impl<'a> vte::Perform for VtePerformer<'a> {
    fn print(&mut self, c: char) {
        self.grid
            .put_char_ex(c, self.attrs.fg, self.attrs.bg, self.attrs.flags);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.grid.line_feed(),
            b'\r' => self.grid.cur_x = 0,
            b'\x08' => {
                // Backspace
                self.grid.cur_x = self.grid.cur_x.saturating_sub(1);
            }
            _ => (),
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        final_byte: char,
    ) {
        let mut params_iter = params.iter();
        let mut get_param = |default| params_iter.next().map(|p| p[0] as usize).unwrap_or(default);

        match final_byte {
            'r' => {
                // DECSTBM - Set Top and Bottom Margins (Scrolling Region)
                let top = get_param(1).saturating_sub(1); // 1-based to 0-based
                let bottom = get_param(self.grid.rows).saturating_sub(1); // 1-based to 0-based

                if top < bottom && bottom < self.grid.rows {
                    self.grid.scroll_top = top;
                    self.grid.scroll_bottom = bottom;
                    self.grid.set_cursor_pos(0, top);
                }
            }
            'm' => {
                // SGR (Select Graphic Rendition)
                if params.is_empty() {
                    *self.attrs = Attrs::default();
                    self.grid.scroll_top = 0;
                    self.grid.scroll_bottom = self.grid.rows.saturating_sub(1);

                    return;
                }
                for p in params.iter() {
                    let n = p[0] as u8;
                    match n {
                        0 => *self.attrs = Attrs::default(),
                        1 => self.attrs.flags.insert(CellFlags::BOLD),
                        2 => self.attrs.flags.insert(CellFlags::FAINT),
                        22 => self.attrs.flags.remove(CellFlags::BOLD | CellFlags::FAINT),

                        30..=37 => self.attrs.fg = ansi_16(n - 30, false),
                        90..=97 => self.attrs.fg = ansi_16(n - 90, true),
                        39 => self.attrs.fg = Attrs::default().fg,

                        40..=47 => self.attrs.bg = ansi_16(n - 40, false),
                        100..=107 => self.attrs.bg = ansi_16(n - 100, true),
                        49 => self.attrs.bg = Attrs::default().bg,
                        _ => {}
                    }
                }
            }
            'A' => {
                // CUU - Cursor Up
                let n = get_param(1);
                self.grid.cur_y = self.grid.cur_y.saturating_sub(n);
            }
            'B' => {
                // CUD - Cursor Down
                let n = get_param(1);
                self.grid.cur_y = (self.grid.cur_y + n).min(self.grid.rows.saturating_sub(1));
            }
            'C' => {
                // CUF - Cursor Forward
                let n = get_param(1);
                self.grid.cur_x = (self.grid.cur_x + n).min(self.grid.cols.saturating_sub(1));
            }
            'D' => {
                // CUB - Cursor Back
                let n = get_param(1);
                self.grid.cur_x = self.grid.cur_x.saturating_sub(n);
            }
            'H' => {
                // CUP - Cursor Position
                let row = get_param(1).saturating_sub(1); // 1-based to 0-based
                let col = get_param(1).saturating_sub(1); // 1-based to 0-based
                self.grid.set_cursor_pos(col, row);
            }
            'J' => {
                // ED - Erase in Display
                match get_param(0) {
                    0 => self.grid.clear_from_cursor(),
                    2 => self.grid.clear_all(),
                    _ => eprintln!("Unhandled ED: {:?}", params),
                }
            }
            'K' => {
                // EL - Erase in Line
                match get_param(0) {
                    0 => self.grid.clear_line_from_cursor(),
                    2 => self.grid.clear_line(),
                    _ => eprintln!("Unhandled EL: {:?}", params),
                }
            }
            _ => {}
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
    attrs: Attrs,
    pub scroll_offset: usize,
}

impl TerminalState {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            grid: ScreenGrid::new(cols, rows, 10_000),
            parser: Parser::new(),
            attrs: Attrs::default(),
            scroll_offset: 0,
        }
    }

    pub fn scroll_viewport(&mut self, delta: i32) {
        let new_offset = self.scroll_offset as i32 - delta;

        self.scroll_offset = new_offset.max(0).min(self.grid.scrollback_len() as i32) as usize;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // When new output arrives, scroll to bottom
        self.scroll_offset = 0;

        let mut performer = VtePerformer {
            grid: &mut self.grid,
            attrs: &mut self.attrs,
        };
        self.parser.advance(&mut performer, bytes);
    }
}
