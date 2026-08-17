use std::fmt::Debug;

use bevy::prelude::*;

use ratatui::Terminal;

use soft_ratatui::embedded_graphics_unicodefonts::{
    mono_8x13_atlas, mono_8x13_bold_atlas, mono_8x13_italic_atlas,
};
use soft_ratatui::{EmbeddedGraphics, SoftBackend};

/// Ratatui context that will set up a window and render the ratatui buffer using a 2D texture,
/// instead of drawing to a terminal buffer.
#[derive(Deref, DerefMut)]
pub struct WindowedContext(Terminal<SoftBackend<EmbeddedGraphics>>);

impl Debug for WindowedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WindowedContext()")
    }
}

impl WindowedContext {
    /// Creates the software-rendered terminal.
    pub fn init() -> Result<Self> {
        let font_regular = mono_8x13_atlas();
        let font_italic = mono_8x13_italic_atlas();
        let font_bold = mono_8x13_bold_atlas();
        let backend = SoftBackend::<EmbeddedGraphics>::new(
            100,
            50,
            font_regular,
            Some(font_bold),
            Some(font_italic),
        );
        let terminal = Terminal::new(backend)?;
        Ok(Self(terminal))
    }
}
