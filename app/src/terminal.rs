use screen_grid::ScreenGrid;
use vte::{Parser, Perform};

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
        };

        self.parser.advance(&mut performer, bytes);
    }
}

struct GridPerform<'a> {
    grid: &'a mut ScreenGrid,
}

impl<'a> Perform for GridPerform<'a> {
    fn print(&mut self, c: char) {
        self.grid.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        if byte == b'\n' {
            self.grid.line_feed();
        }
    }

    // TODO add csi/osc
}
