//! Arrêt propre sur Ctrl-C (SIGINT) pendant une phase d'enrichissement (DEC-153, DEC-156).
//!
//! Unix seulement.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Nombre de SIGINT reçus depuis l'installation de la garde courante.
static RECEIVED: AtomicUsize = AtomicUsize::new(0);

/// Une seule phase par processus (DEC-112).
static PHASE: Mutex<()> = Mutex::new(());

/// Message du second Ctrl-C (design V-006, E-28) : seule source du texte.
pub(crate) const E28: &[u8] =
    "erreur: envoi interrompu : second Ctrl-C reçu : relancez nephila update pour continuer\n"
        .as_bytes();

/// Code de sortie d'une exécution interrompue.
pub(crate) const EXIT_INTERRUPTED: i32 = 130;

/// Nombre de SIGINT reçus depuis l'installation de la garde courante.
pub(crate) fn received() -> usize {
    RECEIVED.load(Ordering::SeqCst)
}

/// Gestionnaire de SIGINT. Il n'appelle que des fonctions sûres en contexte de
/// signal (opérations atomiques, `write`, `_exit`) : aucune allocation, aucun
/// verrou, aucune écriture formatée.
extern "C" fn on_sigint(_: libc::c_int) {
    let n = RECEIVED.fetch_add(1, Ordering::SeqCst) + 1;
    if n >= 2 {
        // SAFETY: `write` et `_exit` sont async-signal-safe ; le tampon est statique.
        unsafe {
            let _ = libc::write(2, E28.as_ptr().cast(), E28.len());
            libc::_exit(EXIT_INTERRUPTED);
        }
    }
}

/// Garde de phase : installe le gestionnaire et rétablit la disposition
/// précédente à sa destruction.
pub(crate) struct Guard {
    old: Option<libc::sigaction>,
    _phase: MutexGuard<'static, ()>,
    #[cfg(test)]
    _test: TestShared,
}

impl Guard {
    pub(crate) fn install() -> Guard {
        // Ordre des verrous : celui des tests d'abord, puis `PHASE`.
        #[cfg(test)]
        let test = test_shared();
        let phase = PHASE.lock().unwrap_or_else(|e| e.into_inner());
        Guard {
            old: install_handler(),
            _phase: phase,
            #[cfg(test)]
            _test: test,
        }
    }
}

fn install_handler() -> Option<libc::sigaction> {
    // SAFETY: structures `sigaction` initialisées à zéro puis remplies ; pointeurs valides.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut old) != 0 {
            return None;
        }
        if old.sa_sigaction == libc::SIG_IGN {
            return None;
        }
        RECEIVED.store(0, Ordering::SeqCst);
        let mut new: libc::sigaction = std::mem::zeroed();
        new.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t;
        new.sa_flags = libc::SA_RESTART;
        if libc::sigemptyset(&mut new.sa_mask) != 0
            || libc::sigaction(libc::SIGINT, &new, std::ptr::null_mut()) != 0
        {
            return None;
        }
        Some(old)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(old) = self.old.take() {
            // SAFETY: `old` est la disposition lue à l'installation.
            unsafe {
                libc::sigaction(libc::SIGINT, &old, std::ptr::null_mut());
            }
        }
        // Une phase suivante sans garde ne doit pas voir le signal de celle-ci.
        RECEIVED.store(0, Ordering::SeqCst);
    }
}

/// Verrou des tests (DEC-112) : la disposition de SIGINT et le compteur sont globaux au processus.
/// Ordre unique : ce verrou, puis `PHASE`. Un fil qui le détient déjà (exclusif ou partagé) ne le
/// reprend pas.
#[cfg(test)]
static TEST_RW: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[cfg(test)]
thread_local! {
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Garde exclusive des tests qui lisent ou posent la disposition de SIGINT, ou envoient SIGINT.
#[cfg(test)]
pub(crate) struct TestExclusive {
    _g: std::sync::RwLockWriteGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestExclusive {
    fn drop(&mut self) {
        HELD.with(|h| h.set(false));
    }
}

#[cfg(test)]
pub(crate) fn test_lock() -> TestExclusive {
    let g = TEST_RW.write().unwrap_or_else(|e| e.into_inner());
    HELD.with(|h| h.set(true));
    TestExclusive { _g: g }
}

/// Garde partagée des tests d'enrichissement sans signal : exclut seulement les tests exclusifs.
#[cfg(test)]
pub(crate) struct TestShared {
    _g: Option<std::sync::RwLockReadGuard<'static, ()>>,
}

#[cfg(test)]
impl Drop for TestShared {
    fn drop(&mut self) {
        if self._g.is_some() {
            HELD.with(|h| h.set(false));
        }
    }
}

/// Sans effet sur un fil qui détient déjà l'une des gardes.
#[cfg(test)]
pub(crate) fn test_shared() -> TestShared {
    if HELD.with(|h| h.get()) {
        return TestShared { _g: None };
    }
    let g = TEST_RW.read().unwrap_or_else(|e| e.into_inner());
    HELD.with(|h| h.set(true));
    TestShared { _g: Some(g) }
}

/// Bloque SIGINT dans le fil appelant (fils de travail).
pub(crate) fn block_in_this_thread() {
    // SAFETY: ensemble de signaux local initialisé par `sigemptyset`.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock() -> TestExclusive {
        test_lock()
    }

    fn current() -> libc::sigaction {
        unsafe {
            let mut a: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(libc::SIGINT, std::ptr::null(), &mut a), 0);
            a
        }
    }

    fn set(a: &libc::sigaction) {
        unsafe {
            assert_eq!(libc::sigaction(libc::SIGINT, a, std::ptr::null_mut()), 0);
        }
    }

    fn blocked() -> bool {
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut set),
                0
            );
            libc::sigismember(&set, libc::SIGINT) == 1
        }
    }

    #[test]
    fn guard_installs_handler_and_restores_previous_disposition() {
        let _t = lock();
        let before = current();
        {
            let _g = Guard::install();
            let during = current();
            assert_eq!(
                during.sa_sigaction,
                on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t
            );
            assert_ne!(during.sa_flags & libc::SA_RESTART, 0);
        }
        let after = current();
        assert_eq!(after.sa_sigaction, before.sa_sigaction);
        // La glibc ajoute SA_RESTORER à toute disposition réinstallée.
        let useful = libc::SA_RESTART | libc::SA_SIGINFO | libc::SA_NODEFER | libc::SA_RESETHAND;
        assert_eq!(after.sa_flags & useful, before.sa_flags & useful);
    }

    #[test]
    fn guard_leaves_ignored_sigint_untouched() {
        let _t = lock();
        /// Rétablit la disposition d'origine même si une assertion échoue.
        struct Restore(libc::sigaction);
        impl Drop for Restore {
            fn drop(&mut self) {
                set(&self.0);
            }
        }
        let _restore = Restore(current());
        let mut ign: libc::sigaction = unsafe { std::mem::zeroed() };
        ign.sa_sigaction = libc::SIG_IGN;
        set(&ign);
        {
            let _g = Guard::install();
            assert_eq!(current().sa_sigaction, libc::SIG_IGN);
        }
        assert_eq!(current().sa_sigaction, libc::SIG_IGN);
    }

    #[test]
    fn handler_counts_and_new_guard_resets() {
        let _t = lock();
        {
            let _g = Guard::install();
            assert_eq!(received(), 0);
            on_sigint(libc::SIGINT);
            assert_eq!(received(), 1);
        }
        assert_eq!(received(), 0);
        let _g = Guard::install();
        assert_eq!(received(), 0);
    }

    #[test]
    fn e28_text_and_exit_code() {
        assert_eq!(
            std::str::from_utf8(E28).unwrap(),
            "erreur: envoi interrompu : second Ctrl-C reçu : relancez nephila update pour continuer\n"
        );
        assert_eq!(EXIT_INTERRUPTED, 130);
    }

    #[test]
    fn block_in_this_thread_blocks_only_that_thread() {
        let _t = lock();
        assert!(!blocked());
        let in_thread = std::thread::spawn(|| {
            block_in_this_thread();
            blocked()
        })
        .join()
        .unwrap();
        assert!(in_thread);
        assert!(!blocked());
    }
}
