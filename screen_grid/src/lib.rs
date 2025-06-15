bitflags::bitflags! {
    #[derive(Default)]
    pub struct CellFlags: u16 {
        const BOLD = 0b0000_0001;
        const ITALIC = 0b0000_0010;
        const UNDERLINE = 0b0000_0100;
        const INVERSE = 0b0000_1000;
        const DIRTY = 0b1000_0000;
    }
}
