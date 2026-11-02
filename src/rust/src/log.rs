//! Minimal syslog logging over /dev/log (no dependencies).

use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);
pub static FOREGROUND: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
pub enum Level {
    Warn,
    Info,
    Debug,
}

pub fn log(level: Level, msg: &str) {
    if matches!(level, Level::Debug) && !VERBOSE.load(Ordering::Relaxed) {
        return;
    }
    let (severity, tag) = match level {
        Level::Warn => (4, "warning"),
        Level::Info => (6, "info"),
        Level::Debug => (7, "debug"),
    };
    let foreground = FOREGROUND.load(Ordering::Relaxed);
    // write!, not eprintln!: a closed stderr must never take the daemon down
    if foreground {
        let _ = writeln!(std::io::stderr(), "[{}] {}", tag, msg);
    }
    // facility "user" (1) * 8 + severity
    let line = format!("<{}>cachedirs2[{}]: {}", 8 + severity, std::process::id(), msg);
    let sent = UnixDatagram::unbound()
        .and_then(|s| s.send_to(line.as_bytes(), "/dev/log"))
        .is_ok();
    if !sent && !foreground {
        let _ = writeln!(std::io::stderr(), "[{}] {}", tag, msg);
    }
}

#[macro_export]
macro_rules! info {
    ($($a:tt)*) => { $crate::log::log($crate::log::Level::Info, &format!($($a)*)) };
}
#[macro_export]
macro_rules! warn {
    ($($a:tt)*) => { $crate::log::log($crate::log::Level::Warn, &format!($($a)*)) };
}
#[macro_export]
macro_rules! debug {
    ($($a:tt)*) => { $crate::log::log($crate::log::Level::Debug, &format!($($a)*)) };
}
