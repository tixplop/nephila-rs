//! Progression `--show-progress` (DEC-053, REQ-043) : une ligne entière
//! `progression: <phase> <p> % (<k>/<n> fichiers)` par palier de 10 % sur stderr, écrite d'un
//! seul `write_all` sous le verrou de stderr. Jamais de `\r` ni de séquence ANSI.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    static CAPTURED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Phase suivie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Extraction,
    Enrichment,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Extraction => "extraction",
            Phase::Enrichment => "enrichissement",
        }
    }
}

/// Active ou non la progression.
pub fn init(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Ligne complète (sans fin de ligne) : seul endroit qui construit une ligne de progression.
pub fn format_line(phase: Phase, k: usize, n: usize) -> String {
    let percent = (k * 100).checked_div(n).unwrap_or(0);
    format!(
        "progression: {} {percent} % ({k}/{n} fichiers)",
        phase.label()
    )
}

fn emit(line: &str) {
    if !enabled() {
        return;
    }
    #[cfg(test)]
    CAPTURED.with(|c| c.borrow_mut().push(line.to_string()));
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(buf.as_bytes());
}

/// Lignes émises par le fil courant depuis le dernier appel (tests de la bibliothèque).
#[cfg(test)]
pub fn take_captured() -> Vec<String> {
    CAPTURED.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Nouveau palier (dizaine de pourcents) si `k` sur `n` dépasse le dernier palier écrit.
fn next_step(k: usize, n: usize, last: u8) -> Option<u8> {
    if n == 0 {
        return None;
    }
    let step = (k.min(n) * 10 / n) as u8;
    (step > last).then_some(step)
}

/// Compteur d'une phase.
pub struct Meter {
    phase: Phase,
    n: usize,
    k: usize,
    step: u8,
}

impl Meter {
    /// Démarre une phase de `n` fichiers ; `None` (aucune ligne) si `n` vaut 0.
    pub fn start(phase: Phase, n: usize) -> Option<Meter> {
        if n == 0 {
            return None;
        }
        emit(&format_line(phase, 0, n));
        Some(Meter {
            phase,
            n,
            k: 0,
            step: 0,
        })
    }

    /// Ajoute `by` fichiers traités (borné à `n`) ; une seule ligne si un palier est franchi.
    pub fn advance(&mut self, by: usize) {
        self.k = self.k.saturating_add(by).min(self.n);
        if let Some(step) = next_step(self.k, self.n, self.step) {
            self.step = step;
            emit(&format_line(self.phase, self.k, self.n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(n: usize, ks: &[usize]) -> Vec<usize> {
        let mut last = 0u8;
        let mut out = vec![0];
        for &k in ks {
            if let Some(s) = next_step(k, n, last) {
                last = s;
                out.push(k);
            }
        }
        out
    }

    #[test]
    fn req043_next_step_n20() {
        let ks: Vec<usize> = (1..=20).collect();
        assert_eq!(steps(20, &ks), (0..=20).step_by(2).collect::<Vec<_>>());
    }

    #[test]
    fn req043_next_step_n3_n1_n10() {
        assert_eq!(steps(3, &[1, 2, 3]), vec![0, 1, 2, 3]);
        assert_eq!(steps(1, &[1]), vec![0, 1]);
        assert_eq!(steps(10, &[4, 8, 10]), vec![0, 4, 8, 10]);
    }

    #[test]
    fn req043_next_step_n1250() {
        let ks: Vec<usize> = (1..=1250).collect();
        let got = steps(1250, &ks);
        assert_eq!(&got[..3], &[0, 125, 250]);
        assert_eq!(got.len(), 11);
        assert_eq!(*got.last().unwrap(), 1250);
        // 251 : palier 2 atteint à 250 déjà ; ⌊100·251/1250⌋ = 20.
        assert_eq!(next_step(251, 1250, 2), None);
    }

    #[test]
    fn req043_format_line() {
        assert_eq!(
            format_line(Phase::Extraction, 1, 3),
            "progression: extraction 33 % (1/3 fichiers)"
        );
        assert_eq!(
            format_line(Phase::Enrichment, 3, 3),
            "progression: enrichissement 100 % (3/3 fichiers)"
        );
    }

    #[test]
    fn req043_meter_zero_and_jumps() {
        init(true);
        take_captured();
        assert!(Meter::start(Phase::Extraction, 0).is_none());
        assert!(take_captured().is_empty());
        let mut m = Meter::start(Phase::Enrichment, 10).unwrap();
        m.advance(4);
        m.advance(0);
        m.advance(100);
        m.advance(1);
        assert_eq!(
            take_captured(),
            vec![
                "progression: enrichissement 0 % (0/10 fichiers)",
                "progression: enrichissement 40 % (4/10 fichiers)",
                "progression: enrichissement 100 % (10/10 fichiers)",
            ]
        );
        init(false);
    }
}
