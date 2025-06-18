use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{config::Config, terminal::TerminalState};
use cosmic_text::ttf_parser;
use glyphon::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
    fontdb::{self, Database},
};
use screen_grid::{CellFlags, ScreenGrid};

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

    /// Finds dirty rows and performs the expensive shaping
    pub fn shape(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        scout_db: Arc<fontdb::Database>,
        fallback_cache: &mut HashMap<char, Option<fontdb::ID>>,
        font_family_cache: &mut HashMap<char, String>,
        term: &mut TerminalState,
    ) -> bool {
        let cursor_visible = term.cursor_visible;
        let (cur_y, cur_x) = {
            let grid = term.grid();
            (grid.cur_y, grid.cur_x)
        };

        let normal_loaded = self.shape_grid(
            font_system,
            swash_cache,
            scout_db.clone(),
            fallback_cache,
            font_family_cache,
            &mut term.normal_grid,
            cursor_visible,
            cur_y,
            cur_x,
        );

        let alternate_loaded = self.shape_grid(
            font_system,
            swash_cache,
            scout_db,
            fallback_cache,
            font_family_cache,
            &mut term.alternate_grid,
            cursor_visible,
            cur_y,
            cur_x,
        );

        normal_loaded || alternate_loaded
    }

    /// Helper function to shape one grid at a time
    fn shape_grid(
        &mut self,
        font_system: &mut FontSystem,
        _swash_cache: &mut SwashCache,
        scout_db: Arc<fontdb::Database>,
        fallback_cache: &mut HashMap<char, Option<fontdb::ID>>,
        font_family_cache: &mut HashMap<char, String>,
        grid: &mut ScreenGrid,
        cursor_visible: bool,
        term_cur_y: usize,
        term_cur_x: usize,
    ) -> bool {
        let mut new_fonts_loaded = false;
        let grid_cols = grid.cols;
        let scrollback_len = grid.scrollback_len();

        for (y, row) in grid.lines.iter_mut().enumerate() {
            if !row.is_dirty {
                continue;
            }

            let line_text = row.text();
            let unique_chars: HashSet<char> = line_text.chars().collect();

            for &c in &unique_chars {
                if c == ' ' || fallback_cache.contains_key(&c) {
                    continue;
                }

                // List some preferred fonts
                let preferred_families = ["Hack Nerd Font Mono", "Symbols Nerd Font"];

                let mut found_face: Option<&fontdb::FaceInfo> = None;

                for family in &preferred_families {
                    let query = fontdb::Query {
                        families: &[fontdb::Family::Name(family)],
                        ..Default::default()
                    };

                    if let Some(id) = scout_db.query(&query) {
                        // We found a font with this preferred family name. Does it have the character?
                        if scout_db
                            .with_face_data(id, |data, idx| {
                                ttf_parser::Face::parse(data, idx)
                                    .map_or(false, |f| f.glyph_index(c).is_some())
                            })
                            .unwrap_or(false)
                        {
                            found_face = scout_db.face(id);
                            break; // Found a good font
                        }
                    }
                }

                if found_face.is_none() {
                    found_face = scout_db.faces().find(|face| {
                        if face.families.iter().any(|(name, _)| name == ".LastResort") {
                            return false;
                        }

                        scout_db
                            .with_face_data(face.id, |data, idx| {
                                ttf_parser::Face::parse(data, idx)
                                    .map_or(false, |f| f.glyph_index(c).is_some())
                            })
                            .unwrap_or(false)
                    });
                }

                let found_id = found_face.map(|face| face.id);

                if let Some(id) = found_id {
                    if let Some(face_info) = scout_db.face(id) {
                        if let Some((family_name, _)) = face_info.families.get(0) {
                            font_family_cache.insert(c, family_name.clone());

                            if font_system.db().face(id).is_none() {
                                if let Some((source, _index)) = scout_db.face_source(id) {
                                    let font_data = match &source {
                                        fontdb::Source::File(path) => std::fs::read(path).ok(),
                                        fontdb::Source::Binary(data) => {
                                            Some(data.as_ref().as_ref().to_vec())
                                        }
                                        fontdb::Source::SharedFile(_, data) => {
                                            Some(data.as_ref().as_ref().to_vec())
                                        }
                                    };

                                    if let Some(data) = font_data {
                                        font_system.db_mut().load_font_data(data);
                                        new_fonts_loaded = true;
                                        log::info!(
                                            "Loaded new font source for '{}' (face id: {})",
                                            c,
                                            id
                                        );
                                    }
                                }
                            }

                            fallback_cache.insert(c, Some(id));
                        } else {
                            // This face has no family name...?
                            fallback_cache.insert(c, None);
                        }
                    }
                } else {
                    log::warn!("Could not find any font for character '{}'", c);
                    fallback_cache.insert(c, None);
                }
            }

            let mut buffer = Buffer::new(
                font_system,
                Metrics::new(self.config.font_size, self.cell_size.1),
            );
            buffer.set_size(
                font_system,
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

                    let current_char_needs_fallback = fallback_cache
                        .get(&cell.ch)
                        .and_then(|opt| Some(opt.is_some()))
                        .unwrap_or(false);
                    let run_start_char_needs_fallback = fallback_cache
                        .get(&run_start_cell.ch)
                        .and_then(|opt| Some(opt.is_some()))
                        .unwrap_or(false);

                    if *cell != *run_start_cell
                        || is_cursor != run_start_cursor
                        || current_char_needs_fallback != run_start_char_needs_fallback
                    {
                        let run_end_byte = line_text.len();
                        if run_end_byte > run_start_byte {
                            let fg = if run_start_cursor {
                                run_start_cell.bg
                            } else {
                                run_start_cell.fg
                            };

                            let run_char = run_start_cell.ch;
                            let mut attrs;

                            if let Some(family_name) = font_family_cache.get(&run_char) {
                                log::info!(
                                    "Char '{}' uses explicit Family::Name('{}')",
                                    run_char,
                                    family_name
                                );
                                attrs = Attrs::new().family(Family::Name(family_name));
                            } else {
                                attrs = self.default_attrs.clone();
                            }

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
                    line_text.push(cell.ch);
                }

                let run_end_byte = line_text.len();
                if run_end_byte > run_start_byte {
                    let fg = if run_start_cursor {
                        run_start_cell.bg
                    } else {
                        run_start_cell.fg
                    };

                    let run_char = run_start_cell.ch;
                    let mut attrs;

                    if let Some(family_name) = font_family_cache.get(&run_char) {
                        log::info!(
                            "FINAL RUN: Char '{}' uses explicit Family::Name('{}')",
                            run_char,
                            family_name
                        );
                        attrs = Attrs::new().family(Family::Name(family_name));
                    } else {
                        attrs = self.default_attrs.clone();
                    }

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

            buffer.set_text(
                font_system,
                &line_text,
                &self.default_attrs,
                Shaping::Advanced,
            );
            buffer.lines[0].set_attrs_list(attrs_list);
            buffer.shape_until_scroll(font_system, true);

            row.render_cache = Some(buffer);
        }

        new_fonts_loaded
    }
}
