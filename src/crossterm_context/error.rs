//! Panic-hook ownership for the terminal session.
use std::{panic, sync::Arc};

type PanicHook = Box<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

#[derive(Default)]
pub(crate) struct PanicHookGuard(Option<Arc<PanicHook>>);

impl PanicHookGuard {
    pub(crate) fn install(cleanup: impl Fn() + Send + Sync + 'static) -> Self {
        let previous = Arc::new(panic::take_hook());
        let panic_hook = Arc::clone(&previous);
        panic::set_hook(Box::new(move |info| {
            cleanup();
            panic_hook(info);
        }));
        Self(Some(previous))
    }

    pub(crate) fn is_installed(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn restore(&mut self) {
        let Some(previous) = self.0.take() else {
            return;
        };

        drop(panic::take_hook());
        match Arc::try_unwrap(previous) {
            Ok(previous) => panic::set_hook(previous),
            Err(previous) => panic::set_hook(Box::new(move |info| previous(info))),
        }
    }
}
