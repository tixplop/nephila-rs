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
#[cfg(not(test))]
static MUTED: AtomicBool = AtomicBool::new(false);
// Sous test, le mute est propre au fil (comme `CAPTURED`, DEC-109, DEC-112) : un test qui tait
// les traces ne gêne pas ses voisins quand ils partagent le processus.
#[cfg(test)]
thread_local! {
    static MUTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn is_muted() -> bool {
    #[cfg(not(test))]
    return MUTED.load(Ordering::Relaxed);
    #[cfg(test)]
    return MUTED.with(|m| m.get());
}
static START: OnceLock<Instant> = OnceLock::new();

/// Active ou non les traces ; la première activation fixe l'instant de départ.
pub fn init(enabled: bool) {
    if enabled {
        START.get_or_init(Instant::now);
    }
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) && !is_muted()
}

/// Fait taire les traces (passes de collecte du mode lot, AC-214-5) : tant que c'est posé,
/// `emit` n'écrit rien et `enabled()` est faux. Seule `emit_unmuted` passe encore.
pub fn mute(muted: bool) {
    #[cfg(not(test))]
    MUTED.store(muted, Ordering::Relaxed);
    #[cfg(test)]
    MUTED.with(|m| m.set(muted));
}

/// Ligne complète (sans fin de ligne) pour un temps écoulé donné.
pub fn format_line(elapsed: Duration, event: &str) -> String {
    format!(
        "trace: [{}.{:03}s] {event}",
        elapsed.as_secs(),
        elapsed.subsec_millis()
    )
}

/// Écrit une trace si elles sont actives et non tues.
pub fn emit(args: fmt::Arguments) {
    if is_muted() {
        return;
    }
    emit_unmuted(args);
}

/// Comme `emit`, même quand les traces sont tues : pour la seule trace de `schema_refused`, que
/// le repli écrirait sinon pendant une passe de collecte pour rien (AC-214-5).
pub fn emit_unmuted(args: fmt::Arguments) {
    #[cfg(test)]
    if capture_event(&args) {
        return;
    }
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let start = *START.get_or_init(Instant::now);
    let mut line = format_line(start.elapsed(), &args.to_string());
    line.push('\n');
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(line.as_bytes());
}

#[cfg(test)]
thread_local! {
    static CAPTURED: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
static CAPTURED_ALL: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn capture_event(args: &fmt::Arguments) -> bool {
    let local = CAPTURED.with(|c| match c.borrow_mut().as_mut() {
        Some(v) => {
            v.push(args.to_string());
            true
        }
        None => false,
    });
    local
        || (ENABLED.load(Ordering::Relaxed)
            && match CAPTURED_ALL
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
            {
                Some(v) => {
                    v.push(args.to_string());
                    true
                }
                None => false,
            })
}

/// Comme `capture`, pour toutes les traces du processus, fils de travail compris, avec les
/// traces activées (`verbose`) ou non : un événement n'est retenu que si `enabled()` est vrai.
/// Le test détient `interrupt::test_lock()`, qui exclut les autres tests d'enrichissement.
/// Une garde remet l'état initial (capture éteinte, traces inactives) même après un panic.
#[cfg(test)]
pub(crate) fn capture_all<R>(verbose: bool, f: impl FnOnce() -> R) -> (R, Vec<String>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            *CAPTURED_ALL.lock().unwrap_or_else(|p| p.into_inner()) = None;
            init(false);
        }
    }
    let _reset = Reset;
    init(verbose);
    *CAPTURED_ALL.lock().unwrap_or_else(|p| p.into_inner()) = Some(Vec::new());
    let r = f();
    let events = CAPTURED_ALL
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or_default();
    (r, events)
}

/// Exécute `f` en rangeant les traces émises par le fil courant (texte après `trace: [..] `),
/// même si les traces sont désactivées (DEC-109).
#[cfg(test)]
pub(crate) fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    // Garde : la capture se termine même si `f` panique.
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            CAPTURED.with(|c| *c.borrow_mut() = None);
        }
    }
    CAPTURED.with(|c| *c.borrow_mut() = Some(Vec::new()));
    let guard = Guard;
    let r = f();
    let events = CAPTURED.with(|c| c.borrow_mut().take()).unwrap_or_default();
    drop(guard);
    (r, events)
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
        Error::Network(_) | Error::Saturated(_) => "réseau",
        Error::InvalidKey(_) => "clé refusée",
        Error::Rejected { .. } => "requête refusée",
        Error::Redirected { .. } => "requête redirigée",
        Error::Truncated { .. } => "tronquée",
        Error::Message(_)
        | Error::Format(_)
        | Error::TooManyCalls { .. }
        | Error::CallsOverCap { .. }
        | Error::BatchItem { .. } => "erreur",
        // Un essai refusé par le plafond part avant toute trace d'appel : il n'arrive jamais ici.
        Error::Capped => unreachable!("un essai refusé par le plafond n'est jamais classé"),
        // Une requête mise de côté part avant toute trace d'appel : elle n'arrive jamais ici.
        Error::Deferred => unreachable!("une requête mise de côté n'est jamais classée"),
        Error::Unsent => unreachable!("un appel arrêté avant tout envoi n'est jamais classé"),
        // Erreur de fin de commande : jamais le résultat d'un appel.
        Error::Interrupted(_) => unreachable!("une fin de commande n'est jamais classée"),
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
    fn dec109_capture_range_les_traces_du_fil() {
        let ((), ev) = capture(|| {
            crate::trace!("a {}", 1);
            crate::trace!("b");
        });
        assert_eq!(ev, vec!["a 1", "b"]);
        let ((), none) = capture(|| {});
        assert!(none.is_empty());
    }

    #[test]
    fn req214_mute_tait_emit_mais_pas_emit_unmuted() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                mute(false);
            }
        }
        let _reset = Reset;
        let ((), ev) = capture(|| {
            mute(true);
            crate::trace!("tue");
            emit_unmuted(format_args!("dit {}", 1));
            mute(false);
            crate::trace!("dite");
        });
        assert_eq!(ev, vec!["dit 1", "dite"]);
    }

    #[test]
    fn req214_classify_batch_item_est_une_erreur() {
        let e = crate::error::Error::BatchItem {
            cause: "expirée".into(),
            refused: None,
        };
        assert_eq!(classify(&e), "erreur");
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
