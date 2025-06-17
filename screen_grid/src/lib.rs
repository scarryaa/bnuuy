use std::collections::VecDeque;

bitflags::bitflags! {
    /// Styles that affect a rendered cell
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CellFlags: u16 {
        const BOLD = 0b0000_0001;
        const ITALIC = 0b0000_0010;
        const UNDERLINE = 0b0000_0100;
        const INVERSE = 0b0000_1000;
        const FAINT = 0b0001_0000;
        const UNDERCURL = 0b0010_0000;
    }
}

/// 24-bit RGB color
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// One printable cell on the screen
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: CellFlags,
    pub link_id: Option<u32>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Rgb(0xC0, 0xC0, 0xC0),
            bg: Rgb(0x00, 0x00, 0x00),
            flags: CellFlags::empty(),
            link_id: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub is_dirty: bool,
}

impl Row {
    pub fn iter(&self) -> std::slice::Iter<'_, Cell> {
        self.cells.iter()
    }
}

pub struct ScreenGrid {
    /// Visible rows * cols (not counting scrollback)
    pub rows: usize,
    pub cols: usize,

    /// The viewport: rows[0..rows) are the screen;
    /// older lines live above in `scrollback`
    lines: VecDeque<Row>,

    /// Cursor position in the visible area
    pub cur_x: usize,
    pub cur_y: usize,

    /// Max scrollback lines kept
    scrollback_capacity: usize,

    pub full_redraw_needed: bool,
    pub scroll_top: usize,
    pub scroll_bottom: usize,

    default_fg: Rgb,
    default_bg: Rgb,
    deferred_wrap: bool,
}

impl ScreenGrid {
    pub fn new(
        cols: usize,
        rows: usize,
        scrollback: usize,
        default_fg: Rgb,
        default_bg: Rgb,
    ) -> Self {
        let mut grid = ScreenGrid {
            rows,
            cols,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cur_x: 0,
            cur_y: 0,
            lines: VecDeque::with_capacity(rows + scrollback),
            scrollback_capacity: scrollback,
            full_redraw_needed: true,
            default_fg,
            default_bg,
            deferred_wrap: false,
        };

        grid.resize(cols, rows);
        grid
    }

    pub fn clear_all_dirty_flags(&mut self) {
        self.full_redraw_needed = false;
        for row in self.lines.iter_mut() {
            row.is_dirty = false;
        }
    }

    /// Write one glyph together with its colours + flags
    pub fn put_char_ex(
        &mut self,
        ch: char,
        fg: Rgb,
        bg: Rgb,
        flags: CellFlags,
        link_id: Option<u32>,
    ) {
        if self.deferred_wrap {
            self.line_feed();
            self.cur_x = 0;
            self.deferred_wrap = false;
        }

        let x = self.cur_x;
        let y = self.cur_y;

        if x < self.cols {
            if let Some(row) = self.visible_row_mut(y) {
                row.cells[x] = Cell {
                    ch,
                    fg,
                    bg,
                    flags,
                    link_id,
                };
                row.is_dirty = true;
            }
        }

        self.advance_cursor();
    }

    /// Clear everything and allocate blank rows
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if self.cols == cols && self.rows == rows {
            return;
        }

        self.cols = cols;
        self.rows = rows;

        let fg = self.default_fg;
        let bg = self.default_bg;

        self.lines.clear();
        for _ in 0..rows {
            self.lines.push_back(blank_row(cols, fg, bg));
        }

        self.cur_x = 0;
        self.cur_y = 0;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.deferred_wrap = false;
        self.full_redraw_needed = true;
    }

    /// Move cursor to a given position
    pub fn set_cursor_pos(&mut self, x: usize, y: usize) {
        if let Some(row) = self.visible_row_mut(self.cur_y) {
            row.is_dirty = true;
        }

        self.deferred_wrap = false;

        self.cur_x = x.min(self.cols.saturating_sub(1));
        self.cur_y = y.min(self.rows.saturating_sub(1));

        if let Some(row) = self.visible_row_mut(self.cur_y) {
            row.is_dirty = true;
        }
    }

    /// Clear the entire line the cursor is on
    pub fn clear_line(&mut self) {
        self.deferred_wrap = false;
        let cols = self.cols;
        let fg = self.default_fg;
        let bg = self.default_bg;

        if let Some(row) = self.visible_row_mut(self.cur_y) {
            *row = blank_row(cols, fg, bg);
            row.is_dirty = true;
        }
    }

    /// Erases from beginning of line to cursor
    pub fn clear_line_to_cursor(&mut self) {
        self.deferred_wrap = false;
        let cur_x = self.cur_x;
        let cur_y = self.cur_y;
        let cols = self.cols;
        let blank_cell = Cell {
            fg: self.default_fg,
            bg: self.default_bg,
            ..Default::default()
        };

        if let Some(row) = self.visible_row_mut(cur_y) {
            for x in 0..=cur_x {
                if x < cols {
                    row.cells[x] = blank_cell.clone();
                }
            }
            row.is_dirty = true;
        }
    }

    /// Clear from the cursor to the end of the line
    pub fn clear_line_from_cursor(&mut self) {
        self.deferred_wrap = false;
        let cur_x = self.cur_x;
        let cols = self.cols;

        let blank_cell = Cell {
            fg: self.default_fg,
            bg: self.default_bg,
            ..Default::default()
        };

        if let Some(row) = self.visible_row_mut(self.cur_y) {
            for x in cur_x..cols {
                row.cells[x] = blank_cell.clone();
            }
            row.is_dirty = true;
        }
    }

    /// Erases from start of screen to cursor
    pub fn clear_to_cursor(&mut self) {
        self.deferred_wrap = false;
        let cur_y = self.cur_y;
        let scroll_top = self.scroll_top;
        let cols = self.cols;
        let fg = self.default_fg;
        let bg = self.default_bg;

        for y in scroll_top..cur_y {
            if let Some(row) = self.visible_row_mut(y) {
                *row = blank_row(cols, fg, bg);
            }
        }

        self.clear_line_to_cursor();
    }

    /// Clear from the cursor to the end of the screen
    pub fn clear_from_cursor(&mut self) {
        self.deferred_wrap = false;
        self.clear_line_from_cursor();

        let cur_y = self.cur_y;
        let scroll_bottom = self.scroll_bottom;
        let cols = self.cols;

        let fg = self.default_fg;
        let bg = self.default_bg;

        for y in (cur_y + 1)..=scroll_bottom {
            if let Some(row) = self.visible_row_mut(y) {
                *row = blank_row(cols, fg, bg);
                row.is_dirty = true;
            }
        }
    }

    /// Clear the entire visible screen and move cursor to (0,0)
    pub fn clear_all(&mut self) {
        let scroll_top = self.scroll_top;
        let scroll_bottom = self.scroll_bottom;
        let cols = self.cols;

        let fg = self.default_fg;
        let bg = self.default_bg;

        for y in scroll_top..=scroll_bottom {
            if let Some(row) = self.visible_row_mut(y) {
                *row = blank_row(cols, fg, bg);
            }
        }

        self.set_cursor_pos(0, self.scroll_top);
        self.deferred_wrap = false;
        self.full_redraw_needed = true;
    }

    /// Inserts `n` blank lines at the cursor's current row
    /// Lines at and below the cursor are pushed down, within the scroll region
    pub fn insert_lines(&mut self, n: usize) {
        self.deferred_wrap = false;
        let y = self.cur_y;

        if y < self.scroll_top || y > self.scroll_bottom {
            return;
        }

        let n = n.min(self.scroll_bottom - y + 1);
        if n == 0 {
            return;
        }

        let fg = self.default_fg;
        let bg = self.default_bg;

        let region_start_idx = self.scrollback_len() + y;
        let region_end_idx = self.scrollback_len() + self.scroll_bottom;

        let lines_slice = self.lines.make_contiguous();
        if region_end_idx >= lines_slice.len() {
            return;
        }

        let affected_region = &mut lines_slice[region_start_idx..=region_end_idx];

        affected_region.rotate_right(n);

        for i in 0..n {
            affected_region[i] = blank_row(self.cols, fg, bg);
        }

        self.full_redraw_needed = true;
    }

    /// Deletes `n` lines at the cursor's current row
    /// Lines below the cursor are pulled up, and blank lines are added at the bottom of the scroll region
    pub fn delete_lines(&mut self, n: usize) {
        self.deferred_wrap = false;
        let y = self.cur_y;

        if y < self.scroll_top || y > self.scroll_bottom {
            return;
        }

        let n = n.min(self.scroll_bottom - y + 1);
        if n == 0 {
            return;
        }

        let fg = self.default_fg;
        let bg = self.default_bg;

        let region_start_idx = self.scrollback_len() + y;
        let region_end_idx = self.scrollback_len() + self.scroll_bottom;

        let lines_slice = self.lines.make_contiguous();
        if region_end_idx >= lines_slice.len() {
            return;
        }

        let affected_region = &mut lines_slice[region_start_idx..=region_end_idx];

        affected_region.rotate_left(n);

        let affected_len = affected_region.len();
        for i in 0..n {
            affected_region[affected_len - 1 - i] = blank_row(self.cols, fg, bg);
        }

        self.full_redraw_needed = true;
    }

    /// Inserts `n` blank characters at the cursor position
    pub fn insert_chars(&mut self, n: usize) {
        self.deferred_wrap = false;
        let y = self.cur_y;
        let x = self.cur_x;
        let cols = self.cols;

        let blank_cell = Cell {
            fg: self.default_fg,
            bg: self.default_bg,
            ..Default::default()
        };

        if let Some(row) = self.visible_row_mut(y) {
            for _ in 0..n {
                if x < cols {
                    row.cells.insert(x, blank_cell.clone());
                    row.cells.truncate(cols);
                }
            }
            row.is_dirty = true;
        }
    }

    /// Deletes `n` characters at the cursor position
    pub fn delete_chars(&mut self, n: usize) {
        self.deferred_wrap = false;
        let y = self.cur_y;
        let x = self.cur_x;
        let cols = self.cols;

        let blank_cell = Cell {
            fg: self.default_fg,
            bg: self.default_bg,
            ..Default::default()
        };

        if let Some(row) = self.visible_row_mut(y) {
            for _ in 0..n {
                if x < row.cells.len() {
                    row.cells.remove(x);
                }
            }

            while row.cells.len() < cols {
                row.cells.push(blank_cell.clone());
            }
            row.is_dirty = true;
        }
    }

    /// Handle \n (line feed)
    pub fn line_feed(&mut self) {
        if self.deferred_wrap {
            self.deferred_wrap = false;
            return;
        }

        if self.cur_y == self.scroll_bottom {
            self.scroll_up(1);
        } else {
            self.cur_y += 1;
        }
    }

    /// Scroll the viewport up by `n` lines
    pub fn scroll_up(&mut self, n: usize) {
        let scrollable_lines_in_region = self.scroll_bottom.saturating_sub(self.scroll_top) + 1;
        let n = n.min(scrollable_lines_in_region);

        if n == 0 {
            return;
        }

        let fg = self.default_fg;
        let bg = self.default_bg;

        let mut scrolled_off_rows = Vec::with_capacity(n);

        for _ in 0..n {
            let top_idx = self.scrollback_len() + self.scroll_top;
            if let Some(removed_row) = self.lines.remove(top_idx) {
                if self.scrollback_capacity > 0 {
                    scrolled_off_rows.push(removed_row);
                }
            }
        }

        for _ in 0..n {
            let bottom_idx = self.scrollback_len() + self.scroll_bottom + 1;
            let clamped_idx = bottom_idx.min(self.lines.len());
            self.lines.insert(clamped_idx, blank_row(self.cols, fg, bg));
        }

        for row in scrolled_off_rows {
            self.push_scrollback(row);
        }

        self.full_redraw_needed = true;
    }

    fn advance_cursor(&mut self) {
        if self.cur_x + 1 >= self.cols {
            self.deferred_wrap = true;
        } else {
            self.cur_x += 1;
        }
    }

    pub fn visible_row(&self, y: usize) -> Option<&Row> {
        let sb = self.scrollback_len();
        self.lines.get(sb + y)
    }

    pub fn visible_row_mut(&mut self, y: usize) -> Option<&mut Row> {
        self.lines.get_mut(self.scrollback_len() + y)
    }

    pub fn scrollback_len(&self) -> usize {
        self.lines.len().saturating_sub(self.rows)
    }

    pub fn get_display_row(&self, y: usize, offset: usize) -> Option<&Row> {
        let total_lines = self.lines.len();
        let top_visible_idx = total_lines.saturating_sub(self.rows);
        let requested_idx = top_visible_idx.saturating_sub(offset);

        self.lines.get(requested_idx + y)
    }

    fn push_scrollback(&mut self, row: Row) {
        self.lines.push_front(row);

        while self.lines.len() > self.rows + self.scrollback_capacity {
            self.lines.pop_front();
        }
    }
}

fn blank_row(cols: usize, default_fg: Rgb, default_bg: Rgb) -> Row {
    let blank_cell = Cell {
        fg: default_fg,
        bg: default_bg,
        ..Default::default()
    };
    let cells = std::iter::repeat(blank_cell).take(cols).collect();

    Row {
        cells,
        is_dirty: true,
    }
}
