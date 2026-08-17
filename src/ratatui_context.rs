use bevy::prelude::*;

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
pub type DefaultContext = crate::context::CrosstermSession;

#[cfg(feature = "windowed")]
pub type DefaultContext = crate::context::WindowedContext;

/// A Bevy resource wrapping the terminal that Ratatui draws to. Bring it into systems to draw each
/// frame, like the example below.
///
/// With the Crossterm backend this is the `context::CrosstermSession` that owns the terminal: it
/// acquired raw mode, the alternate screen, and the optional modes, and it restores all of them
/// exactly once when it is dropped (on `AppExit`, when the `App` is dropped, or on the panic path).
/// [`RatatuiPlugins`](crate::RatatuiPlugins) and [`RatatuiContext::init`] construct exactly the
/// same value.
///
/// # Example
///
/// ```rust
/// use bevy::prelude::*;
/// use bevy_ratatui::RatatuiContext;
///
/// fn draw_system(mut context: ResMut<RatatuiContext>) -> Result {
///     context.draw(|frame| {
///         // Draw widgets etc. to the terminal
///     })?;
///     Ok(())
/// }
/// ```
#[derive(Resource, Deref, DerefMut, Debug)]
pub struct RatatuiContext(pub DefaultContext);

impl RatatuiContext {
    /// Acquires the terminal with default `SessionOptions`. Dropping the returned value restores
    /// the terminal.
    #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
    pub fn init() -> Result<Self> {
        use crate::context::{CrosstermSession, SessionOptions};

        Ok(Self(CrosstermSession::acquire(SessionOptions::default())?))
    }

    /// Creates the windowed software-rendering context.
    #[cfg(feature = "windowed")]
    pub fn init() -> Result<Self> {
        Ok(Self(crate::context::WindowedContext::init()?))
    }
}
