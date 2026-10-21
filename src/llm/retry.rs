//! Nouveaux essais d'un appel LLM dont la réponse est non conforme (REQ-038, DEC-046).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

/// Appelle `call` (argument = numéro d'essai, de 1 à `retries + 1`) tant qu'il rend
/// `Err(Format)`, qu'il reste des essais et que `stop` est faux. Un `Ok` ou une autre
/// erreur termine aussitôt. Une erreur de format finale reçoit, si `retries >= 1`, le suffixe
/// ` (<k> essais sur <t>)`.
pub fn with_format_retries<T>(
    retries: u32,
    stop: &AtomicBool,
    mut call: impl FnMut(u32) -> Result<T>,
) -> Result<T> {
    let total = retries.saturating_add(1);
    let mut attempt = 1;
    loop {
        match call(attempt) {
            Err(Error::Format(m)) => {
                if attempt >= total || stop.load(Ordering::SeqCst) {
                    return Err(if retries == 0 {
                        Error::Format(m)
                    } else {
                        Error::Format(format!("{m} ({attempt} essais sur {total})"))
                    });
                }
                attempt += 1;
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(retries: u32, stop: &AtomicBool, ok_at: Option<u32>) -> (Result<u32>, u32) {
        let mut calls = 0;
        let r = with_format_retries(retries, stop, |n| {
            calls += 1;
            assert_eq!(n, calls);
            if Some(n) == ok_at {
                Ok(n)
            } else {
                Err(Error::Format("mauvais".into()))
            }
        });
        (r, calls)
    }

    #[test]
    fn first_try_success() {
        let (r, c) = run(2, &AtomicBool::new(false), Some(1));
        assert_eq!((r, c), (Ok(1), 1));
    }

    #[test]
    fn success_on_third() {
        let (r, c) = run(2, &AtomicBool::new(false), Some(3));
        assert_eq!((r, c), (Ok(3), 3));
    }

    #[test]
    fn three_format_failures_get_suffix() {
        let (r, c) = run(2, &AtomicBool::new(false), None);
        assert_eq!(c, 3);
        assert_eq!(r, Err(Error::Format("mauvais (3 essais sur 3)".into())));
    }

    #[test]
    fn zero_retries_is_one_call_and_untouched() {
        let (r, c) = run(0, &AtomicBool::new(false), None);
        assert_eq!((r, c), (Err(Error::Format("mauvais".into())), 1));
    }

    #[test]
    fn network_error_stops_without_suffix() {
        let mut calls = 0;
        let r: Result<()> = with_format_retries(2, &AtomicBool::new(false), |n| {
            calls += 1;
            if n == 1 {
                Err(Error::Format("x".into()))
            } else {
                Err(Error::Network("net".into()))
            }
        });
        assert_eq!(calls, 2);
        assert_eq!(r, Err(Error::Network("net".into())));
    }

    #[test]
    fn stop_during_first_try() {
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        let r: Result<()> = with_format_retries(2, &stop, |_| {
            calls += 1;
            stop.store(true, Ordering::SeqCst);
            Err(Error::Format("mauvais".into()))
        });
        assert_eq!(calls, 1);
        assert_eq!(r, Err(Error::Format("mauvais (1 essais sur 3)".into())));
    }

    #[test]
    fn stop_already_raised_still_makes_one_call() {
        let (r, c) = run(2, &AtomicBool::new(true), Some(1));
        assert_eq!((r, c), (Ok(1), 1));
    }

    fn truncated() -> Error {
        Error::Truncated("tronquée".into())
    }

    #[test]
    fn req046_truncated_first_try_is_one_call_without_suffix() {
        let mut calls = 0;
        let r: Result<()> = with_format_retries(2, &AtomicBool::new(false), |_| {
            calls += 1;
            Err(truncated())
        });
        assert_eq!(calls, 1);
        assert_eq!(r, Err(truncated()));
        assert_eq!(r.unwrap_err().to_string(), "tronquée");
    }

    #[test]
    fn req046_format_then_truncated_stops_without_suffix() {
        let mut calls = 0;
        let r: Result<()> = with_format_retries(5, &AtomicBool::new(false), |n| {
            calls += 1;
            if n == 1 {
                Err(Error::Format("mauvais".into()))
            } else {
                Err(truncated())
            }
        });
        assert_eq!(calls, 2);
        assert_eq!(r, Err(truncated()));
    }

    #[test]
    fn req046_truncated_with_zero_retries_is_one_call() {
        let mut calls = 0;
        let r: Result<()> = with_format_retries(0, &AtomicBool::new(false), |_| {
            calls += 1;
            Err(truncated())
        });
        assert_eq!((calls, r), (1, Err(truncated())));
    }
}
