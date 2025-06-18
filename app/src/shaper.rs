use std::sync::Arc;

use crate::{config::Config, terminal::TerminalState};
use glyphon::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight, fontdb::Database,
};
use screen_grid::{CellFlags, ScreenGrid};

pub struct Shaper {
    font_system: FontSystem,
    default_attrs: Attrs<'static>,
    config: Arc<Config>,
    cell_size: (f32, f32),
}

impl Shaper {
    pub fn new(config: Arc<Config>) -> Self {
        let mut db = Database::new();

        // TODO share this logic between renderer and shaper
        db.load_font_data(Vec::from(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/fonts/HackNerdFontMono-Regular.ttf"
        ))));
        db.load_font_data(Vec::from(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/fonts/HackNerdFontMono-Italic.ttf"
        ))));
        db.load_font_data(Vec::from(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/fonts/HackNerdFontMono-Bold.ttf"
        ))));
        db.load_font_data(Vec::from(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/fonts/HackNerdFontMono-BoldItalic.ttf"
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
            font_system,
            default_attrs,
            config,
            cell_size,
        }
    }

    /// Finds dirty rows and performs the expensive shaping
    pub fn shape(&mut self, term: &mut TerminalState) {
        let cursor_visible = term.cursor_visible;
        let (cur_y, cur_x) = {
            let grid = term.grid();
            (grid.cur_y, grid.cur_x)
        };

        self.shape_grid(&mut term.normal_grid, cursor_visible, cur_y, cur_x);
        self.shape_grid(&mut term.alternate_grid, cursor_visible, cur_y, cur_x);
    }

    /// Helper function to shape one grid at a time
    fn shape_grid(
        &mut self,
        grid: &mut ScreenGrid,
        cursor_visible: bool,
        term_cur_y: usize,
        term_cur_x: usize,
    ) {
        let grid_cols = grid.cols;
        let scrollback_len = grid.scrollback_len();

        for (y, row) in grid.lines.iter_mut().enumerate() {
            if !row.is_dirty {
                continue;
            }

            let mut buffer = Buffer::new(
                &mut self.font_system,
                Metrics::new(self.config.font_size, self.cell_size.1),
            );
            buffer.set_size(
                &mut self.font_system,
                Some(grid_cols as f32 * self.cell_size.0),
                Some(self.cell_size.1),
            );

            let mut line_text = String::with_capacity(grid_cols);
            let mut attrs_list = glyphon::AttrsList::new(&self.default_attrs);

            let logical_cursor_y = scrollback_len + term_cur_y;
            let is_cursor_on_this_line = cursor_visible && y == logical_cursor_y;

            if !row.cells.is_empty() {
                let mut run_start_byte = 0;
                let mut run_start_cell = &row.cells[0];
                let mut run_start_cursor = is_cursor_on_this_line && 0 == term_cur_x;

                for (i, cell) in row.cells.iter().enumerate() {
                    let is_cursor = is_cursor_on_this_line && i == term_cur_x;

                    if *cell != *run_start_cell || is_cursor != run_start_cursor {
                        let run_end_byte = line_text.len();
                        if run_end_byte > run_start_byte {
                            let fg = if run_start_cursor {
                                run_start_cell.bg
                            } else {
                                run_start_cell.fg
                            };
                            let mut attrs = self
                                .default_attrs
                                .clone()
                                .color(glyphon::Color::rgba(fg.0, fg.1, fg.2, 0xFF));
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
                    line_text.push(cell.ch);
                }

                let run_end_byte = line_text.len();
                if run_end_byte > run_start_byte {
                    let fg = if run_start_cursor {
                        run_start_cell.bg
                    } else {
                        run_start_cell.fg
                    };
                    let mut attrs = self
                        .default_attrs
                        .clone()
                        .color(glyphon::Color::rgba(fg.0, fg.1, fg.2, 0xFF));
                    if run_start_cell.flags.contains(CellFlags::ITALIC) {
                        attrs = attrs.style(Style::Italic);
                    }
                    if run_start_cell.flags.contains(CellFlags::BOLD) {
                        attrs = attrs.weight(Weight::BOLD);
                    }
                    attrs_list.add_span(run_start_byte..run_end_byte, &attrs);
                }
            }

            buffer.set_text(
                &mut self.font_system,
                &line_text,
                &self.default_attrs,
                Shaping::Advanced,
            );
            buffer.lines[0].set_attrs_list(attrs_list);
            buffer.shape_until_scroll(&mut self.font_system, true);

            row.render_cache = Some(buffer);
        }
    }
}
