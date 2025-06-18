use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{config::Config, terminal::TerminalState};
use glyphon::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight,
    fontdb::{self, Database},
};
use screen_grid::{CellFlags, Row};

pub struct Shaper {
    default_attrs: Attrs<'static>,
    config: Arc<Config>,
    cell_size: (f32, f32),
}

impl Shaper {
    pub fn new(config: Arc<Config>) -> Self {
        let mut db = Database::new();

        db.load_font_data(Vec::from(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/fonts/HackNerdFontMono-Regular.ttf"
        ))));
        db.set_monospace_family("Hack Nerd Font Mono");

        let mut font_system = FontSystem::new_with_locale_and_db("en-US".into(), db);
        let default_attrs = Attrs::new().family(Family::Monospace);

        let mut temp_buffer = Buffer::new(
            &mut font_system,
            Metrics::new(config.font_size, config.font_size),
        );
        temp_buffer.set_text(&mut font_system, "W", &default_attrs, Shaping::Advanced);
        let cell_w = temp_buffer.layout_runs().next().unwrap().line_w;
        let cell_size = (cell_w, config.font_size);

        Self {
            default_attrs,
            config,
            cell_size,
        }
    }

    /// Finds and shapes a limited number of dirty rows, prioritizing the visible area.
    /// Returns `true` if there are still more dirty rows left after its work is done.
    pub fn shape_budgeted(
        &mut self,
        font_system: &mut FontSystem,
        fallback_cache: &mut HashMap<char, bool>,
        term: &mut TerminalState,
        line_budget: usize,
    ) -> bool {
        let mut lines_shaped = 0;

        let cursor_visible = term.cursor_visible;
        let (cur_y, cur_x, grid_cols, scrollback_len) = {
            let grid = term.grid();
            (grid.cur_y, grid.cur_x, grid.cols, grid.scrollback_len())
        };

        let grid_mut = term.grid_mut();

        let viewport_start = scrollback_len;
        let buffer_len = grid_mut.lines.len();

        for y in viewport_start..buffer_len {
            if let Some(row) = grid_mut.lines.get_mut(y) {
                if row.is_dirty {
                    if lines_shaped >= line_budget {
                        return true;
                    }

                    self.shape_single_row(
                        font_system,
                        fallback_cache,
                        row,
                        grid_cols,
                        cursor_visible,
                        scrollback_len,
                        cur_y,
                        cur_x,
                        y,
                    );
                    lines_shaped += 1;
                }
            }
        }

        if lines_shaped < line_budget {
            for y in (0..viewport_start).rev() {
                if let Some(row) = grid_mut.lines.get_mut(y) {
                    if row.is_dirty {
                        if lines_shaped >= line_budget {
                            return true;
                        }

                        self.shape_single_row(
                            font_system,
                            fallback_cache,
                            row,
                            grid_cols,
                            cursor_visible,
                            scrollback_len,
                            cur_y,
                            cur_x,
                            y,
                        );
                        lines_shaped += 1;
                    }
                }
            }
        }

        false
    }

    /// Helper function to contain the logic for shaping one row.
    fn shape_single_row(
        &mut self,
        font_system: &mut FontSystem,
        fallback_cache: &mut HashMap<char, bool>,
        row: &mut Row,
        grid_cols: usize,
        cursor_visible: bool,
        scrollback_len: usize,
        term_cur_y: usize,
        term_cur_x: usize,
        y: usize,
    ) {
        let main_font_id = {
            let query = fontdb::Query {
                families: &[Family::Name("Hack Nerd Font Mono")],
                ..Default::default()
            };
            font_system.db().query(&query)
        };

        let line_text = row.text();
        let unique_chars: HashSet<char> = line_text.chars().collect();

        for &c in &unique_chars {
            if c == ' ' || fallback_cache.contains_key(&c) {
                continue;
            }

            let mut needs_fallback = true;
            if let Some(id) = main_font_id {
                let main_font_has_glyph = font_system
                    .db()
                    .with_face_data(id, |data, index| {
                        glyphon::cosmic_text::ttf_parser::Face::parse(data, index)
                            .map_or(false, |f| f.glyph_index(c).is_some())
                    })
                    .unwrap_or(false);

                if main_font_has_glyph {
                    needs_fallback = false;
                }
            }
            fallback_cache.insert(c, needs_fallback);
        }

        let mut buffer = row.render_cache.take().unwrap_or_else(|| {
            Buffer::new(
                font_system,
                Metrics::new(self.config.font_size, self.cell_size.1),
            )
        });

        buffer.set_size(
            font_system,
            Some(grid_cols as f32 * self.cell_size.0),
            Some(self.cell_size.1),
        );

        buffer.set_text(
            font_system,
            &line_text,
            &self.default_attrs,
            Shaping::Advanced,
        );

        let mut attrs_list = glyphon::AttrsList::new(&self.default_attrs);
        let is_cursor_on_this_line = cursor_visible && y == (scrollback_len + term_cur_y);

        if !row.cells.is_empty() {
            let mut run_start_byte = 0;
            let mut run_start_cell = &row.cells[0];
            let mut run_start_cursor = is_cursor_on_this_line && 0 == term_cur_x;
            let mut current_byte = 0;

            for (i, cell) in row.cells.iter().enumerate() {
                let is_cursor = is_cursor_on_this_line && i == term_cur_x;
                let char_len = cell.ch.len_utf8();

                let current_char_needs_fallback =
                    fallback_cache.get(&cell.ch).copied().unwrap_or(false);
                let run_start_char_needs_fallback = fallback_cache
                    .get(&run_start_cell.ch)
                    .copied()
                    .unwrap_or(false);

                let attrs_changed = cell.fg != run_start_cell.fg
                    || cell.bg != run_start_cell.bg
                    || cell.flags != run_start_cell.flags;

                if attrs_changed
                    || is_cursor != run_start_cursor
                    || current_char_needs_fallback != run_start_char_needs_fallback
                {
                    let run_end_byte = current_byte;
                    if run_end_byte > run_start_byte {
                        let fg = if run_start_cursor {
                            run_start_cell.bg
                        } else {
                            run_start_cell.fg
                        };
                        let mut attrs = if run_start_char_needs_fallback {
                            Attrs::new()
                        } else {
                            self.default_attrs.clone()
                        };
                        attrs = attrs.color(glyphon::Color::rgba(fg.0, fg.1, fg.2, 0xFF));
                        if run_start_cell.flags.contains(CellFlags::ITALIC) {
                            attrs = attrs.style(Style::Italic);
                        }
                        if run_start_cell.flags.contains(CellFlags::BOLD) {
                            attrs = attrs.weight(Weight::BOLD);
                        }
                        attrs_list.add_span(run_start_byte..run_end_byte, &attrs);
                    }
                    run_start_byte = run_end_byte;
                    run_start_cell = cell;
                    run_start_cursor = is_cursor;
                }
                current_byte += char_len;
            }

            let run_end_byte = current_byte;
            if run_end_byte > run_start_byte {
                let fg = if run_start_cursor {
                    run_start_cell.bg
                } else {
                    run_start_cell.fg
                };
                let run_start_char_needs_fallback = fallback_cache
                    .get(&run_start_cell.ch)
                    .copied()
                    .unwrap_or(false);
                let mut attrs = if run_start_char_needs_fallback {
                    Attrs::new()
                } else {
                    self.default_attrs.clone()
                };
                attrs = attrs.color(glyphon::Color::rgba(fg.0, fg.1, fg.2, 0xFF));
                if run_start_cell.flags.contains(CellFlags::ITALIC) {
                    attrs = attrs.style(Style::Italic);
                }
                if run_start_cell.flags.contains(CellFlags::BOLD) {
                    attrs = attrs.weight(Weight::BOLD);
                }
                attrs_list.add_span(run_start_byte..run_end_byte, &attrs);
            }
        }

        buffer.lines[0].set_attrs_list(attrs_list);
        buffer.shape_until_scroll(font_system, true);

        row.render_cache = Some(buffer);
        row.is_dirty = false;
    }
}
