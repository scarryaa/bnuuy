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
        const DIRTY = 0b1000_0000;
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
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Rgb(0xC0, 0xC0, 0xC0),
            bg: Rgb(0x00, 0x00, 0x00),
            flags: CellFlags::DIRTY,
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
}

impl ScreenGrid {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
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
        };
        grid.resize(cols, rows);
        grid
    }

    pub fn clear_all_dirty_flags(&mut self) {
        for row in self.lines.iter_mut() {
            for cell in row.cells.iter_mut() {
                cell.flags.remove(CellFlags::DIRTY);
            }
        }
    }

    /// Write one glyph together with its colours + flags
    pub fn put_char_ex(&mut self, ch: char, fg: Rgb, bg: Rgb, flags: CellFlags) {
        let x = self.cur_x;
        let y = self.cur_y;

        if x < self.cols {
            if let Some(row) = self.visible_row_mut(y) {
                row.cells[x] = Cell {
                    ch,
                    fg,
                    bg,
                    flags: flags | CellFlags::DIRTY,
                };
                row.is_dirty = true;
            }
        }
        self.advance_cursor();
    }

    /// Clear everything and allocate blank rows
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows = rows;

        self.lines.clear();
        for _ in 0..rows {
            self.lines.push_back(Self::blank_row(cols));
        }
        self.cur_x = 0;
        self.cur_y = 0;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.full_redraw_needed = true;
    }

    /// Move cursor to a given position
    pub fn set_cursor_pos(&mut self, x: usize, y: usize) {
        if let Some(row) = self.visible_row_mut(self.cur_y) {
            row.is_dirty = true;
        }

        self.cur_x = x.min(self.cols.saturating_sub(1));
        self.cur_y = y.min(self.rows.saturating_sub(1));

        if let Some(row) = self.visible_row_mut(self.cur_y) {
            row.is_dirty = true;
        }
    }

    /// Clear the entire line the cursor is on.
    pub fn clear_line(&mut self) {
        let cols = self.cols;
        if let Some(row) = self.visible_row_mut(self.cur_y) {
            *row = Self::blank_row(cols);
            row.is_dirty = true;
        }
    }

    /// Clear from the cursor to the end of the line.
    pub fn clear_line_from_cursor(&mut self) {
        let cur_x = self.cur_x;
        let cols = self.cols;
        if let Some(row) = self.visible_row_mut(self.cur_y) {
            for x in cur_x..cols {
                row.cells[x] = Cell::default();
            }
            row.is_dirty = true;
        }
    }

    /// Clear from the cursor to the end of the screen.
    pub fn clear_from_cursor(&mut self) {
        self.clear_line_from_cursor();

        let cur_y = self.cur_y;
        let rows = self.rows;
        let cols = self.cols;

        for y in (cur_y + 1)..rows {
            if let Some(row) = self.visible_row_mut(y) {
                *row = Self::blank_row(cols);
            }
        }

        self.full_redraw_needed = true;
    }

    /// Clear the entire visible screen and move cursor to (0,0).
    pub fn clear_all(&mut self) {
        let rows = self.rows;
        let cols = self.cols;

        for y in 0..rows {
            if let Some(row) = self.visible_row_mut(y) {
                *row = Self::blank_row(cols);
            }
        }
        self.set_cursor_pos(0, 0);
        self.full_redraw_needed = true;
    }

    /// Inserts `n` blank lines at the cursor's current row
    /// Lines at and below the cursor are pushed down
    pub fn insert_lines(&mut self, n: usize) {
        let y = self.cur_y;
        let bottom = self.scroll_bottom;
        let cols = self.cols;
        let sb_len = self.scrollback_len();

        for _ in 0..n {
            // Remove the last line from scrolling region to make space
            self.lines.remove(sb_len + bottom);
            self.lines.insert(sb_len + y, Self::blank_row(cols));
        }
        self.full_redraw_needed = true;
    }

    /// Deletes `n` lines at the cursor's current row
    /// Lines below the cursor are pulled up
    pub fn delete_lines(&mut self, n: usize) {
        let y = self.cur_y;
        let bottom = self.scroll_bottom;
        let cols = self.cols;
        let sb_len = self.scrollback_len();

        for _ in 0..n {
            // Remove the line at the cursor
            self.lines.remove(sb_len + y);

            // Add a new blank line at the bottom
            self.lines.insert(sb_len + bottom, Self::blank_row(cols));
        }
        self.full_redraw_needed = true;
    }

    /// Inserts `n` blank characters at the cursor position
    pub fn insert_chars(&mut self, n: usize) {
        let y = self.cur_y;
        let x = self.cur_x;
        let cols = self.cols;

        if let Some(row) = self.visible_row_mut(y) {
            for _ in 0..n {
                if x < cols {
                    row.cells.insert(x, Cell::default());
                    row.cells.truncate(cols);
                }
            }
            row.is_dirty = true;
        }
    }

    /// Deletes `n` characters at the cursor position
    pub fn delete_chars(&mut self, n: usize) {
        let y = self.cur_y;
        let x = self.cur_x;
        let cols = self.cols;

        if let Some(row) = self.visible_row_mut(y) {
            for _ in 0..n {
                if x < row.cells.len() {
                    row.cells.remove(x);
                }
            }

            // Add blank cells at the end to fill the space
            while row.cells.len() < cols {
                row.cells.push(Cell::default());
            }
            row.is_dirty = true;
        }
    }

    /// Write `ch` at cursor and advance
    pub fn put_char(&mut self, ch: char) {
        let x = self.cur_x;
        let y = self.cur_y;
        let cols = self.cols;

        if x < cols {
            if let Some(row) = self.visible_row_mut(y) {
                let cell = &mut row.cells[x];
                row.is_dirty = true;
                *cell = Cell {
                    ch,
                    flags: CellFlags::DIRTY,
                    ..*cell
                };
                row.is_dirty = true;
            }
        }

        self.advance_cursor();
    }

    /// Handle \n (line feed)
    pub fn line_feed(&mut self) {
        if let Some(row) = self.visible_row_mut(self.cur_y) {
            row.is_dirty = true;
        }

        if self.cur_y == self.scroll_bottom {
            // We are at the bottom of the scroll region, so scroll the region up
            self.scroll_up(1);
        } else if self.cur_y + 1 < self.rows {
            // We are not at the bottom, just move the cursor down
            self.cur_y += 1;
        }

        if let Some(row) = self.visible_row_mut(self.cur_y) {
            row.is_dirty = true;
        }
    }

    /// Scroll the viewport up by `n` lines
    pub fn scroll_up(&mut self, n: usize) {
        // Check how many lines we can actually scroll
        let scrollable_lines_in_region = self.scroll_bottom - self.scroll_top + 1;
        let n = n.min(scrollable_lines_in_region);

        if n == 0 {
            return;
        }

        let sb_len = self.scrollback_len();
        let top_idx = sb_len + self.scroll_top;
        let bottom_idx = sb_len + self.scroll_bottom;

        let drained_rows: Vec<Row> = self.lines.drain(top_idx..top_idx + n).collect();

        // Push the drained rows to scrollback history
        for row in drained_rows {
            self.push_scrollback(row);
        }

        // Add `n` new blank lines at the bottom of the scrolling region
        for _ in 0..n {
            self.lines
                .insert(bottom_idx - n + 1, Self::blank_row(self.cols));
        }

        self.full_redraw_needed = true;
    }

    fn advance_cursor(&mut self) {
        self.cur_x += 1;
        if self.cur_x >= self.cols {
            self.cur_x = 0;
            self.line_feed();
        }
    }

    fn blank_row(cols: usize) -> Row {
        let cells = std::iter::repeat_with(Cell::default).take(cols).collect();

        Row {
            cells,
            is_dirty: true,
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
