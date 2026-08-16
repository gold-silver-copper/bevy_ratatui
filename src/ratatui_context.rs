use bevy::prelude::*;

use super::context_trait::TerminalContext;

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
pub type DefaultContext = crate::context::CrosstermContext;

#[cfg(feature = "windowed")]
pub type DefaultContext = crate::context::WindowedContext;

/// A bevy Resource that wraps [ratatui::Terminal], setting up the terminal context when
/// initialized (i.e. entering raw mode), and can be brought into Bevy systems to interact with
/// Ratatui. [ContextPlugin](crate::context::ContextPlugin) owns the terminal lifecycle and restores
/// the prior terminal state when the application exits. For example, use this resource to draw to
/// the terminal each frame, like the below example.
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
    /// Initializes the selected terminal backend.
    ///
    /// When called outside [`ContextPlugin`](crate::context::ContextPlugin), the caller is
    /// responsible for calling [`restore`](Self::restore).
    pub fn init() -> Result<Self> {
        Ok(Self(DefaultContext::init()?))
    }

    /// Restores the selected terminal backend.
    pub fn restore() -> Result {
        DefaultContext::restore()
    }
}
