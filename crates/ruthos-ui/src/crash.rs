//! Internal errors are contained, not fatal.
//!
//! A panic inside a window's UI code is caught at the window boundary: the
//! details go to `crash.log` in the data folder (with a backtrace) and to the
//! log, the viewer that failed is closed, and the database window shows a
//! dialog. The application keeps running.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static DEBUG_FIRED: AtomicBool = AtomicBool::new(false);

/// Developer option: `RUTHOS_DEBUG_PANIC=<point>` panics once at that point
/// so the error handling can be exercised.
pub fn debug_panic(point: &str) {
    if DEBUG_FIRED.load(Ordering::Relaxed) {
        return;
    }
    if std::env::var("RUTHOS_DEBUG_PANIC").map(|v| v == point).unwrap_or(false) {
        DEBUG_FIRED.store(true, Ordering::Relaxed);
        panic!("debug panic requested at {point}");
    }
}

static LAST: Mutex<Option<String>> = Mutex::new(None);
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Install the panic hook. Call once, after the data folder is known.
pub fn install(log_path: PathBuf) {
    if let Ok(mut p) = LOG_PATH.lock() {
        *p = Some(log_path);
    }
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let msg = payload_text(info.payload());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        let text = format!("{msg} (at {location})");
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("internal error: {text}\n{backtrace}");
        if let Some(path) = LOG_PATH.lock().ok().and_then(|g| g.clone()) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                use std::io::Write;
                let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let _ = writeln!(f, "[{stamp}] {text}\n{backtrace}\n");
            }
        }
        if let Ok(mut last) = LAST.lock() {
            *last = Some(text);
        }
        default(info);
    }));
}

/// Where panics are written, once installed.
pub fn log_path() -> Option<PathBuf> {
    LOG_PATH.lock().ok().and_then(|g| g.clone())
}

/// Run `f`; a panic inside becomes an error message instead of unwinding
/// further.
pub fn guarded<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Ok(r),
        Err(payload) => {
            let recorded = LAST.lock().ok().and_then(|mut g| g.take());
            Err(recorded.unwrap_or_else(|| payload_text(&*payload)))
        }
    }
}

fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown internal error".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_turns_a_panic_into_an_error() {
        assert_eq!(guarded(|| 3), Ok(3));
        let err = guarded(|| -> i32 { panic!("boom {}", 7) }).unwrap_err();
        assert!(err.contains("boom 7"), "{err}");
    }
}
