//! Démarrage des fils de travail (DEC-109) : le lanceur crée un fil dans la portée de l'appelant ;
//! `start_workers` en démarre jusqu'à `wanted`, dans l'ordre, et s'arrête au premier échec.

use std::io;
use std::thread::{Builder, Scope};

/// Travail d'un fil.
pub(crate) type Work<'scope> = Box<dyn FnOnce() + Send + 'scope>;

/// Crée un fil dans `scope` à partir du `Builder` préparé (nom, pile) et du travail.
pub(crate) trait Launcher: Sync {
    fn launch<'scope, 'env>(
        &self,
        scope: &'scope Scope<'scope, 'env>,
        builder: Builder,
        work: Work<'scope>,
    ) -> io::Result<()>;
}

/// Lanceur de production : `Builder::spawn_scoped`.
pub(crate) struct Spawner;

impl Launcher for Spawner {
    fn launch<'scope, 'env>(
        &self,
        scope: &'scope Scope<'scope, 'env>,
        builder: Builder,
        work: Work<'scope>,
    ) -> io::Result<()> {
        builder.spawn_scoped(scope, work).map(|_| ())
    }
}

/// Démarre jusqu'à `wanted` fils ; rend le nombre de fils démarrés et l'erreur du premier échec.
pub(crate) fn start_workers<'scope, 'env, L: Launcher>(
    scope: &'scope Scope<'scope, 'env>,
    wanted: usize,
    launcher: &L,
    mut builder: impl FnMut() -> Builder,
    mut work: impl FnMut() -> Work<'scope>,
) -> (usize, Option<io::Error>) {
    for started in 0..wanted {
        if let Err(e) = launcher.launch(scope, builder(), work()) {
            return (started, Some(e));
        }
    }
    (wanted, None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Lanceur de test : échoue au k-ième appel (à partir de 1), délègue sinon.
    pub(crate) struct FailAt {
        pub k: usize,
        pub calls: AtomicUsize,
    }

    impl FailAt {
        pub fn new(k: usize) -> Self {
            FailAt {
                k,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Launcher for FailAt {
        fn launch<'scope, 'env>(
            &self,
            scope: &'scope Scope<'scope, 'env>,
            builder: Builder,
            work: Work<'scope>,
        ) -> io::Result<()> {
            if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.k {
                return Err(io::Error::other("refus simulé"));
            }
            Spawner.launch(scope, builder, work)
        }
    }

    #[test]
    fn dec109_arret_au_premier_echec() {
        let ran = AtomicUsize::new(0);
        let l = FailAt::new(3);
        let (n, e) = std::thread::scope(|s| {
            start_workers(s, 5, &l, Builder::new, || {
                Box::new(|| {
                    ran.fetch_add(1, Ordering::SeqCst);
                })
            })
        });
        assert_eq!(n, 2);
        assert_eq!(e.unwrap().to_string(), "refus simulé");
        assert_eq!(ran.load(Ordering::SeqCst), 2);
        assert_eq!(l.calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn dec109_tous_demarres() {
        let l = FailAt::new(0);
        let (n, e) =
            std::thread::scope(|s| start_workers(s, 3, &l, Builder::new, || Box::new(|| {})));
        assert_eq!((n, e.is_none()), (3, true));
    }
}
