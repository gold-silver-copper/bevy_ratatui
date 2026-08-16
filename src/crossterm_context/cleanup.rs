use std::{
    cell::Cell,
    io::{self, stdout},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use bevy::prelude::*;
use ratatui::crossterm::{
    ExecutableCommand, cursor,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};

use crate::RatatuiContext;

use super::kitty::{KittyEnabled, disable_kitty_protocol};
#[cfg(feature = "mouse")]
use super::mouse::{MouseEnabled, disable_mouse_capture};

thread_local! {
    static LOCKED_CLEANUP: Cell<*const CleanupState> = const { Cell::new(ptr::null()) };
}

#[derive(Debug, Default)]
struct CleanupState {
    locked: AtomicBool,
    closed: AtomicBool,
    kitty: AtomicBool,
    #[cfg(feature = "mouse")]
    mouse: AtomicBool,
    alternate_screen: AtomicBool,
    cursor: AtomicBool,
    raw_mode: AtomicBool,
}

#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct CleanupHandle(Arc<CleanupState>);

impl CleanupHandle {
    pub(crate) fn setup<T>(&self, setup: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.with_exclusive(|| {
            self.ensure_open()?;
            setup()
        })
    }

    pub(crate) fn enable_kitty(&self, enable: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        self.track_enable(&self.0.kitty, enable)
    }

    #[cfg(feature = "mouse")]
    pub(crate) fn enable_mouse(&self, enable: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        self.track_enable(&self.0.mouse, enable)
    }

    pub(crate) fn enter_alternate_screen(
        &self,
        enter: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.track_enable(&self.0.alternate_screen, enter)
    }

    pub(crate) fn mark_cursor_may_be_hidden(&self) -> io::Result<()> {
        self.track_enable(&self.0.cursor, || Ok(()))
    }

    pub(crate) fn enable_raw_mode(
        &self,
        enable: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.track_enable(&self.0.raw_mode, enable)
    }

    pub(crate) fn disable_kitty(&self) -> io::Result<()> {
        self.with_exclusive(|| attempt_cleanup(&self.0.kitty, disable_kitty_protocol))
    }

    #[cfg(feature = "mouse")]
    pub(crate) fn disable_mouse(&self) -> io::Result<()> {
        self.with_exclusive(|| attempt_cleanup(&self.0.mouse, disable_mouse_capture))
    }

    pub(crate) fn run(&self) -> io::Result<()> {
        self.with_exclusive(|| {
            self.0.closed.store(true, Ordering::Relaxed);
            let mut first_error = None;

            record_error(
                &mut first_error,
                attempt_cleanup(&self.0.kitty, disable_kitty_protocol),
            );
            #[cfg(feature = "mouse")]
            record_error(
                &mut first_error,
                attempt_cleanup(&self.0.mouse, disable_mouse_capture),
            );
            record_error(
                &mut first_error,
                attempt_cleanup(&self.0.alternate_screen, || {
                    stdout().execute(LeaveAlternateScreen)?;
                    Ok(())
                }),
            );
            record_error(
                &mut first_error,
                attempt_cleanup(&self.0.cursor, || {
                    stdout().execute(cursor::Show)?;
                    Ok(())
                }),
            );
            record_error(
                &mut first_error,
                attempt_cleanup(&self.0.raw_mode, disable_raw_mode),
            );

            first_error.map_or(Ok(()), Err)
        })
    }

    fn track_enable(
        &self,
        state: &AtomicBool,
        enable: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.with_exclusive(|| {
            self.ensure_open()?;
            state.store(true, Ordering::Relaxed);
            enable()
        })
    }

    fn ensure_open(&self) -> io::Result<()> {
        if self.0.closed.load(Ordering::Relaxed) {
            return Err(io::Error::other("terminal cleanup has already started"));
        }

        Ok(())
    }

    fn with_exclusive<T>(&self, action: impl FnOnce() -> T) -> T {
        let state = Arc::as_ptr(&self.0);

        LOCKED_CLEANUP.with(|locked_cleanup| {
            if locked_cleanup.get() == state {
                return action();
            }

            while self
                .0
                .locked
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                std::thread::yield_now();
            }

            struct Unlock<'a> {
                state: &'a CleanupState,
                locked_cleanup: &'a Cell<*const CleanupState>,
                previous: *const CleanupState,
            }

            impl Drop for Unlock<'_> {
                fn drop(&mut self) {
                    self.locked_cleanup.set(self.previous);
                    self.state.locked.store(false, Ordering::Release);
                }
            }

            let previous = locked_cleanup.replace(state);
            let _unlock = Unlock {
                state: &self.0,
                locked_cleanup,
                previous,
            };

            action()
        })
    }
}

fn attempt_cleanup(state: &AtomicBool, cleanup: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    if !state.swap(false, Ordering::Relaxed) {
        return Ok(());
    }

    cleanup()
}

fn record_error(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(err) = result
        && first_error.is_none()
    {
        *first_error = Some(err);
    }
}

pub(crate) fn report_cleanup_error(action: &str, result: io::Result<()>) {
    if let Err(err) = result {
        eprintln!("Failed to {action}: {err}");
    }
}

/// Plugin responsible for restoring terminal state in the correct order when exiting.
///
/// If raw mode, the alternate view, and the Kitty protocol are disabled in the wrong order, it can
/// cause issues for the terminal buffer after the application exits.
pub struct CleanupPlugin;

impl Plugin for CleanupPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CleanupHandle>()
            .add_systems(Last, cleanup);
    }
}

fn cleanup(mut exit: MessageReader<AppExit>, mut commands: Commands) {
    if exit.read().next().is_none() {
        return;
    }

    commands.remove_resource::<KittyEnabled>();
    #[cfg(feature = "mouse")]
    commands.remove_resource::<MouseEnabled>();
    commands.remove_resource::<RatatuiContext>();
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Barrier, mpsc},
        time::Duration,
    };

    use super::*;

    #[test]
    fn cleanup_action_runs_once_after_activation() {
        let action = AtomicBool::new(true);
        let calls = Cell::new(0);

        attempt_cleanup(&action, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        attempt_cleanup(&action, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();

        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn failed_cleanup_action_is_not_retried_out_of_order() {
        let action = AtomicBool::new(true);
        let calls = Cell::new(0);

        assert!(
            attempt_cleanup(&action, || {
                calls.set(calls.get() + 1);
                Err(io::Error::other("cleanup failed"))
            })
            .is_err()
        );
        attempt_cleanup(&action, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();

        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn concurrent_state_changes_wait_for_the_active_sequence() {
        let cleanup = CleanupHandle::default();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));

        let first_cleanup = cleanup.clone();
        let first_entered = Arc::clone(&entered);
        let first_release = Arc::clone(&release);
        let first = std::thread::spawn(move || {
            first_cleanup.with_exclusive(|| {
                first_entered.wait();
                first_release.wait();
            });
        });

        entered.wait();
        let (done_tx, done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            cleanup.track_enable(&cleanup.0.kitty, || Ok(())).unwrap();
            done_tx.send(()).unwrap();
        });

        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release.wait();
        first.join().unwrap();
        second.join().unwrap();
        done_rx.recv().unwrap();
    }

    #[test]
    fn cleanup_prevents_later_state_activation() {
        let cleanup = CleanupHandle::default();

        cleanup.run().unwrap();

        assert!(cleanup.track_enable(&cleanup.0.kitty, || Ok(())).is_err());
    }
}
