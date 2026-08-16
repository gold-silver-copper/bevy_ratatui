//! Panic-hook ownership for the terminal session.
use std::{panic, sync::Arc};

type PanicHook = Box<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

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

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        process::{Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    const CHILD_ENV: &str = "BEVY_RATATUI_PANIC_HOOK_GUARD_TEST_CHILD";
    const TEST_NAME: &str =
        "crossterm_context::error::tests::normal_drop_restores_the_previous_hook";
    const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn normal_drop_restores_the_previous_hook() {
        if env::var_os(CHILD_ENV).is_some() {
            run_restoration_probe();
            return;
        }

        // Panic hooks are process-global. Run the assertion in its own test process so parallel
        // unit tests that intentionally panic cannot observe this temporary hook.
        let mut child = Command::new(env::current_exe().expect("find unit test executable"))
            .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn panic-hook restoration probe");

        let deadline = Instant::now() + CHILD_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll panic-hook restoration probe") {
                break status;
            }
            if Instant::now() >= deadline {
                child
                    .kill()
                    .expect("kill timed-out panic-hook restoration probe");
                let _ = child.wait();
                panic!("panic-hook restoration probe timed out after {CHILD_TIMEOUT:?}");
            }
            thread::sleep(Duration::from_millis(10));
        };

        assert!(status.success(), "panic-hook restoration probe failed");
    }

    fn run_restoration_probe() {
        let original = panic::take_hook();
        let previous_calls = Arc::new(AtomicUsize::new(0));
        let previous_hook_calls = Arc::clone(&previous_calls);
        panic::set_hook(Box::new(move |_| {
            previous_hook_calls.fetch_add(1, Ordering::Relaxed);
        }));

        let cleanup_calls = Arc::new(AtomicUsize::new(0));
        let hook_cleanup_calls = Arc::clone(&cleanup_calls);
        let guard = PanicHookGuard::install(move || {
            hook_cleanup_calls.fetch_add(1, Ordering::Relaxed);
        });
        drop(guard);

        let panic_result = panic::catch_unwind(|| panic!("restored-hook probe"));

        drop(panic::take_hook());
        panic::set_hook(original);

        assert!(panic_result.is_err());
        assert_eq!(previous_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            cleanup_calls.load(Ordering::Relaxed),
            0,
            "the installed cleanup wrapper remained active after guard drop"
        );
    }
}
