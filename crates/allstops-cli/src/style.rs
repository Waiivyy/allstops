//! Minimal terminal styling: colour only on a TTY and only when NO_COLOR is
//! unset (https://no-color.org).

use std::io::IsTerminal;
use std::sync::OnceLock;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::io::stdout().is_terminal()
    })
}

fn paint(code: &str, s: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}

pub fn warn(s: &str) -> String {
    paint("33", s)
}

pub fn good(s: &str) -> String {
    paint("32", s)
}

pub fn bad(s: &str) -> String {
    paint("31", s)
}

pub fn dim(s: &str) -> String {
    paint("2", s)
}
