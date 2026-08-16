use bevy::prelude::*;

use super::context_trait::TerminalContext;

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
pub type DefaultContext = crate::context::CrosstermContext;

#[cfg(feature = "windowed")]
pub type DefaultContext = crate::context::WindowedContext;

/// A bevy Resource that wraps [ratatui::Terminal] and can be brought into Bevy systems to interact
/// with Ratatui. When initialized directly, dropping this resource restores the terminal. When
/// created by [`ContextPlugin`](crate::context::ContextPlugin), the plugin takes ownership of the
/// complete terminal lifecycle so optional terminal modes can be cleaned up in order. For example,
/// use this resource to draw to the terminal each frame, like the below example.
///
/// # Example
///
/// ```rust
/// use bevy::prelude::*;
/// use bevy_ratatui::RatatuiContext;
///
/// fn draw_system(mut context: ResMut<RatatuiContext>) {
///     context.draw(|frame| {
///         // Draw widgets etc. to the terminal
///     });
/// }
/// ```
#[derive(Resource, Deref, DerefMut, Debug)]
pub struct RatatuiContext(pub DefaultContext);

impl RatatuiContext {
    pub fn init() -> Result<Self> {
        Ok(Self(DefaultContext::init()?))
    }
}
