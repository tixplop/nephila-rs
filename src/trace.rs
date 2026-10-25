//! Traces `--verbose` (DEC-032, REQ-026) : une ligne `trace: [<s>.<mmm>s] <événement>` par
//! événement sur stderr, écrite d'un seul `write_all` sous le verrou de stderr.

use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Longueur maximale d'un message d'erreur dans une trace.
pub const MAX_MESSAGE_CHARS: usize = 120;

static ENABLED: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();

/// Active ou non les traces ; la première activation fixe l'instant de départ.
pub fn init(enabled: bool) {
    if enabled {
        START.get_or_init(Instant::now);
    }
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Ligne complète (sans fin de ligne) pour un temps écoulé donné.
pub fn format_line(elapsed: Duration, event: &str) -> String {
    format!(
        "trace: [{}.{:03}s] {event}",
        elapsed.as_secs(),
        elapsed.subsec_millis()
    )
}

/// Écrit une trace si elles sont actives.
pub fn emit(args: fmt::Arguments) {
    if !enabled() {
        return;
    }
    let start = *START.get_or_init(Instant::now);
    let mut line = format_line(start.elapsed(), &args.to_string());
    line.push('\n');
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(line.as_bytes());
}

/// Tronque `msg` à `max` caractères (suffixe `…`), sur une ligne.
pub fn short(msg: &str, max: usize) -> String {
    let flat: String = msg
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

/// Résultat d'un appel, d'après la variante de l'erreur (DEC-032).
pub fn classify(e: &crate::error::Error) -> String {
    use crate::error::Error;
    let label = match e {
        Error::Timeout(_) => "délai dépassé",
        Error::Network(_) => "réseau",
        Error::InvalidKey(_) => "clé refusée",
        Error::Rejected { .. } => "requête refusée",
        Error::Truncated(_) => "tronquée",
        Error::Message(_) | Error::Format(_) | Error::TooManyCalls { .. } => "erreur",
    };
    label.to_string()
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::trace::emit(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req026_format_line() {
        assert_eq!(
            format_line(Duration::from_millis(1234), "x y"),
            "trace: [1.234s] x y"
        );
        assert_eq!(
            format_line(Duration::from_millis(5), "e"),
            "trace: [0.005s] e"
        );
    }

    #[test]
    fn req026_short_truncates_on_char_boundary() {
        let s = "é".repeat(130);
        let t = short(&s, 120);
        assert_eq!(t.chars().count(), 121);
        assert!(t.ends_with('…'));
        assert_eq!(short("abc", 120), "abc");
        assert_eq!(short(&"a".repeat(120), 120), "a".repeat(120));
        assert_eq!(short("a\nb", 120), "a b");
    }
}
