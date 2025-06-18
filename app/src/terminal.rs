use std::{collections::HashMap, sync::Arc};

use screen_grid::{CellFlags, Rgb, ScreenGrid};
use vte::Parser;

use crate::config::Config;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ActiveScreen {
    Normal,
    Alternate,
}

#[derive(Clone, Copy)]
struct Attrs {
    fg: Rgb,
    bg: Rgb,
    flags: CellFlags,
}

impl Attrs {
    fn from_config(config: &Config) -> Self {
        Self {
            fg: Rgb(
                config.colors.foreground.0,
                config.colors.foreground.1,
                config.colors.foreground.2,
            ),
            bg: Rgb(
                config.colors.background.0,
                config.colors.background.1,
                config.colors.background.2,
            ),
            flags: CellFlags::empty(),
        }
    }
}

struct VtePerformer<'a> {
    normal_grid: &'a mut ScreenGrid,
    alternate_grid: &'a mut ScreenGrid,
    active_screen: &'a mut ActiveScreen,

    attrs: &'a mut Attrs,
    cursor_visible: &'a mut bool,
    current_link_id: &'a mut Option<u32>,
    links: &'a mut HashMap<u32, String>,
    next_link_id: &'a mut u32,
    config: Arc<Config>,
}

impl<'a> VtePerformer<'a> {
    fn grid_mut(&mut self) -> &mut ScreenGrid {
        match *self.active_screen {
            ActiveScreen::Normal => self.normal_grid,
            ActiveScreen::Alternate => self.alternate_grid,
        }
    }
}

impl<'a> vte::Perform for VtePerformer<'a> {
    fn print(&mut self, c: char) {
        let attrs = *self.attrs;
        let link_id = *self.current_link_id;

        self.grid_mut()
            .put_char_ex(c, attrs.fg, attrs.bg, attrs.flags, link_id);
    }

    fn execute(&mut self, byte: u8) {
        let grid = self.grid_mut();
        if let Some(row) = grid.visible_row_mut(grid.cur_y) {
            row.is_dirty = true;
        }

        match byte {
            // Newline and Index move down one line
            b'\n' | 0x84 => grid.line_feed(),

            // NEL moves down one line AND to column 0
            0x85 => {
                grid.line_feed();
                grid.cur_x = 0;
            }

            b'\r' => grid.cur_x = 0,
            b'\x08' => {
                grid.cur_x = grid.cur_x.saturating_sub(1);
            }
            _ => (),
        }

        if let Some(row) = grid.visible_row_mut(grid.cur_y) {
            row.is_dirty = true;
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // We only care about OSC 8 for hyperlinks (for now?)
        if params.get(0) != Some(&&b"8"[..]) {
            return;
        }

        let params_str = params.get(1).map(|p| std::str::from_utf8(p).unwrap_or(""));
        let url = params.get(2).map(|p| std::str::from_utf8(p).unwrap_or(""));

        match (params_str, url) {
            (Some(_), Some("")) => {
                // End of link: `OSC 8 ;; ST`
                *self.current_link_id = None;
            }
            (Some(_), Some(url)) => {
                // Start of link: `OSC 8 ; params ; url ST`
                let id =
                    if let Some((found_id, _)) = self.links.iter().find(|(_, val)| **val == *url) {
                        *found_id
                    } else {
                        // It's a new URL, add it

                        let new_id = *self.next_link_id;
                        self.links.insert(new_id, url.to_string());
                        *self.next_link_id += 1;
                        new_id
                    };

                *self.current_link_id = Some(id);
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _c: char) {
        // TODO utilize this later
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        final_byte: char,
    ) {
        let mut params_iter = params.iter();
        let mut get_param = |default| params_iter.next().map(|p| p[0] as usize).unwrap_or(default);

        if intermediates.get(0) == Some(&b'?') {
            if let Some(p) = params.iter().next() {
                // Check for 1049, code for alt screen with clear
                if p[0] == 1049 {
                    match final_byte {
                        'h' => {
                            // Enter Alternate Screen
                            *self.active_screen = ActiveScreen::Alternate;
                            // Clear the alternate screen before use
                            self.grid_mut().clear_all();
                        }
                        'l' => {
                            // Leave Alternate Screen
                            *self.active_screen = ActiveScreen::Normal;
                            // Make sure cursor is visible when returning
                            *self.cursor_visible = true;
                        }
                        _ => {}
                    }

                    self.grid_mut().full_redraw_needed = true;
                    return;
                }
            }

            match final_byte {
                'h' => {
                    // DECSET - Turn mode ON
                    if get_param(0) == 25 {
                        *self.cursor_visible = true;
                        let grid = self.grid_mut();
                        if let Some(row) = grid.visible_row_mut(grid.cur_y) {
                            row.is_dirty = true;
                        }
                    }
                }
                'l' => {
                    // DECRST - Turn mode OFF
                    if get_param(0) == 25 {
                        *self.cursor_visible = false;
                        let grid = self.grid_mut();
                        if let Some(row) = grid.visible_row_mut(grid.cur_y) {
                            row.is_dirty = true;
                        }
                    }
                }
                _ => {}
            }

            return;
        }

        match final_byte {
            'r' => {
                // DECSTBM - Set Scrolling Region
                let grid = self.grid_mut();

                if params.is_empty() {
                    // No parameters -- reset to full screen
                    grid.scroll_top = 0;
                    grid.scroll_bottom = grid.rows.saturating_sub(1);
                    log::debug!("DECSTBM - Resetting scroll region to full");
                } else {
                    let top = params
                        .iter()
                        .nth(0)
                        .and_then(|p| p.get(0))
                        .map(|&v| v as usize)
                        .unwrap_or(1)
                        .saturating_sub(1);

                    let bottom = params
                        .iter()
                        .nth(1)
                        .and_then(|p| p.get(0))
                        .map(|&v| v as usize)
                        .unwrap_or(grid.rows)
                        .saturating_sub(1);

                    if top < bottom && bottom < grid.rows {
                        log::debug!(
                            "DECSTBM - Set Scrolling Region: top={}, bottom={}",
                            top + 1,
                            bottom + 1
                        );
                        grid.scroll_top = top;
                        grid.scroll_bottom = bottom;
                        grid.set_cursor_pos(0, 0);
                    }
                }
            }
            'm' => {
                // SGR - Select Graphic Rendition
                if params.is_empty() {
                    *self.attrs = Attrs::from_config(&self.config);
                    return;
                }

                let mut param_iter = params.iter();

                while let Some(p) = param_iter.next() {
                    let n = p[0] as u16;

                    match n {
                        0 => *self.attrs = Attrs::from_config(&self.config),
                        1 => self.attrs.flags.insert(CellFlags::BOLD),
                        2 => self.attrs.flags.insert(CellFlags::FAINT),
                        3 => self.attrs.flags.insert(CellFlags::ITALIC),
                        4 => {
                            // `4:x` is a Kitty/VTE extension for styled underlines
                            self.attrs
                                .flags
                                .remove(CellFlags::UNDERLINE | CellFlags::UNDERCURL);
                            let style = if p.len() > 1 { p[1] } else { 1 };
                            match style {
                                1 => self.attrs.flags.insert(CellFlags::UNDERLINE), // `4` or `4:1`
                                3 => self.attrs.flags.insert(CellFlags::UNDERCURL), // `4:3`
                                0 => {} // `4:0` is "no underline"
                                _ => self.attrs.flags.insert(CellFlags::UNDERLINE),
                            }
                        }
                        7 => self.attrs.flags.insert(CellFlags::INVERSE),
                        22 => self.attrs.flags.remove(CellFlags::BOLD | CellFlags::FAINT),
                        23 => self.attrs.flags.remove(CellFlags::ITALIC),
                        24 => self
                            .attrs
                            .flags
                            .remove(CellFlags::UNDERLINE | CellFlags::UNDERCURL),
                        27 => self.attrs.flags.remove(CellFlags::INVERSE),

                        30..=37 => self.attrs.fg = ansi_16((n - 30) as u8, false),
                        90..=97 => self.attrs.fg = ansi_16((n - 90) as u8, true),
                        39 => self.attrs.fg = Attrs::from_config(&self.config).fg,

                        40..=47 => self.attrs.bg = ansi_16((n - 40) as u8, false),
                        100..=107 => self.attrs.bg = ansi_16((n - 100) as u8, true),
                        49 => self.attrs.bg = Attrs::from_config(&self.config).bg,

                        38 => {
                            // Set foreground color (extended)
                            if let Some(spec) = param_iter.next() {
                                match spec[0] {
                                    5 => {
                                        // 256-color
                                        if let Some(color_val) = param_iter.next() {
                                            self.attrs.fg = ansi_256_to_rgb(color_val[0] as u8);
                                        }
                                    }
                                    2 => {
                                        // 24-bit True Color
                                        if let (Some(r), Some(g), Some(b)) = (
                                            param_iter.next(),
                                            param_iter.next(),
                                            param_iter.next(),
                                        ) {
                                            self.attrs.fg = Rgb(r[0] as u8, g[0] as u8, b[0] as u8);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        48 => {
                            // Set background color (extended)
                            if let Some(spec) = param_iter.next() {
                                match spec[0] {
                                    5 => {
                                        // 256-color
                                        if let Some(color_val) = param_iter.next() {
                                            self.attrs.bg = ansi_256_to_rgb(color_val[0] as u8);
                                        }
                                    }
                                    2 => {
                                        // 24-bit True Color
                                        if let (Some(r), Some(g), Some(b)) = (
                                            param_iter.next(),
                                            param_iter.next(),
                                            param_iter.next(),
                                        ) {
                                            self.attrs.bg = Rgb(r[0] as u8, g[0] as u8, b[0] as u8);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            'A' => {
                // CUU - Cursor Up
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                } // Treat 0 as 1
                grid.cur_y = grid.cur_y.saturating_sub(n).max(grid.scroll_top);
            }
            'B' => {
                // CUD - Cursor Down
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                } // Treat 0 as 1
                grid.cur_y = (grid.cur_y + n).min(grid.scroll_bottom);
            }
            'C' => {
                // CUF - Cursor Forward
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                } // Treat 0 as 1
                grid.cur_x = (grid.cur_x + n).min(grid.cols.saturating_sub(1));
            }
            'D' => {
                // CUB - Cursor Back
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                } // Treat 0 as 1
                grid.cur_x = grid.cur_x.saturating_sub(n);
            }
            'H' => {
                // CUP - Cursor Position
                let grid = self.grid_mut();
                let row = get_param(1).saturating_sub(1); // 1-based to 0-based
                let col = get_param(1).saturating_sub(1); // 1-based to 0-based
                grid.set_cursor_pos(col, row);
            }
            'J' => {
                // ED - Erase in Display
                let grid = self.grid_mut();
                match get_param(0) {
                    0 => grid.clear_from_cursor(),
                    1 => { /* TODO Erase from start of screen to cursor */ }
                    2 => grid.clear_all(),
                    _ => eprintln!("Unhandled ED: {:?}", params),
                }
            }
            'K' => {
                // EL - Erase in Line
                let grid = self.grid_mut();
                match get_param(0) {
                    0 => grid.clear_line_from_cursor(),
                    1 => { /* TODO Erase from start of line to cursor */ }
                    2 => grid.clear_line(),
                    _ => eprintln!("Unhandled EL: {:?}", params),
                }
            }
            'X' => {
                // ECH - Erase Character

                let blank_cell = screen_grid::Cell {
                    ch: ' ',
                    fg: self.attrs.fg,
                    bg: self.attrs.bg,
                    flags: screen_grid::CellFlags::empty(),
                    link_id: *self.current_link_id,
                };

                let grid = self.grid_mut();
                let n = get_param(1);
                let x = grid.cur_x;
                let y = grid.cur_y;

                if let Some(row) = grid.visible_row_mut(y) {
                    for i in 0..n {
                        if x + i < row.cells.len() {
                            row.cells[x + i] = blank_cell.clone();
                        }
                    }
                    row.is_dirty = true;
                }
            }
            '@' => {
                // ICH - Insert Character
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                }
                grid.insert_chars(n);
            }
            'L' => {
                // IL - Insert Line
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                }
                grid.insert_lines(n);
            }
            'M' => {
                // DL - Delete Line
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                }
                grid.delete_lines(n);
            }
            'P' => {
                // DCH - Delete Character
                let grid = self.grid_mut();
                let mut n = get_param(1);
                if n == 0 {
                    n = 1;
                }
                grid.delete_chars(n);
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
    pub normal_grid: ScreenGrid,
    pub alternate_grid: ScreenGrid,
    pub active_screen: ActiveScreen,

    parser: Parser,
    attrs: Attrs,
    pub scroll_offset: usize,
    pub cursor_visible: bool,
    config: Arc<Config>,
    pub links: HashMap<u32, String>,
    next_link_id: u32,
    current_link_id: Option<u32>,
    pub is_dirty: bool,
}

impl TerminalState {
    pub fn new(cols: usize, rows: usize, config: Arc<Config>) -> Self {
        let default_attrs = Attrs::from_config(&config);
        let default_fg = default_attrs.fg;
        let default_bg = default_attrs.bg;

        let normal_grid = ScreenGrid::new(cols, rows, 10_000, default_fg, default_bg);
        let alternate_grid = ScreenGrid::new(cols, rows, 0, default_fg, default_bg);

        Self {
            normal_grid,
            alternate_grid,
            active_screen: ActiveScreen::Normal,
            parser: Parser::new(),
            attrs: default_attrs,
            scroll_offset: 0,
            cursor_visible: true,
            links: HashMap::new(),
            next_link_id: 1,
            current_link_id: None,
            config,
            is_dirty: true,
        }
    }

    pub fn grid(&self) -> &ScreenGrid {
        match self.active_screen {
            ActiveScreen::Normal => &self.normal_grid,
            ActiveScreen::Alternate => &self.alternate_grid,
        }
    }

    pub fn grid_mut(&mut self) -> &mut ScreenGrid {
        match self.active_screen {
            ActiveScreen::Normal => &mut self.normal_grid,
            ActiveScreen::Alternate => &mut self.alternate_grid,
        }
    }

    pub fn scroll_viewport(&mut self, delta: i32) {
        if self.active_screen == ActiveScreen::Alternate {
            return;
        }

        let grid = &mut self.normal_grid;
        let new_offset = self.scroll_offset as i32 - delta;
        let new_offset = new_offset.max(0).min(grid.scrollback_len() as i32) as usize;

        if self.scroll_offset != new_offset {
            self.scroll_offset = new_offset;
            self.is_dirty = true;
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        self.is_dirty = true;
        if self.active_screen == ActiveScreen::Normal {
            self.scroll_offset = 0;
        }

        let mut performer = VtePerformer {
            normal_grid: &mut self.normal_grid,
            alternate_grid: &mut self.alternate_grid,
            active_screen: &mut self.active_screen,
            attrs: &mut self.attrs,
            cursor_visible: &mut self.cursor_visible,
            current_link_id: &mut self.current_link_id,
            links: &mut self.links,
            next_link_id: &mut self.next_link_id,
            config: self.config.clone(),
        };

        self.parser.advance(&mut performer, bytes);
    }

    pub fn clear_dirty(&mut self) {
        self.is_dirty = false;
        self.grid_mut().clear_all_dirty_flags();
    }

    pub fn get_link_at(&self, col: usize, row: usize) -> Option<u32> {
        self.grid()
            .get_display_row(row, self.scroll_offset)
            .and_then(|r| r.cells.get(col))
            .and_then(|c| c.link_id)
    }
}

fn ansi_256_to_rgb(color_code: u8) -> Rgb {
    match color_code {
        // Standard 16 ANSI colors
        0..=15 => {
            let bright = color_code > 7;
            let idx = if bright { color_code - 8 } else { color_code };
            ansi_16(idx, bright)
        }
        // 6x6x6 color cube
        16..=231 => {
            let code = color_code - 16;
            let r = (code / 36) * 51;
            let g = ((code % 36) / 6) * 51;
            let b = (code % 6) * 51;
            Rgb(r, g, b)
        }
        // Grayscale ramp
        232..=255 => {
            let gray = (color_code - 232) * 10 + 8;
            Rgb(gray, gray, gray)
        }
    }
}
