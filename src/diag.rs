//! Portée de diagnostic par test (DEC-112) : compteurs propres à un test, hérités par les fils
//! que le code lance pour lui. Hors tests, `add` et `Inherit` sont vides : aucun coût.

/// Compteur de diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Counter {
    ExtractThreads,
    WalkThreads,
    HashThreads,
    ParsersCreated,
    Preparations,
}

#[cfg(test)]
pub(crate) use active::*;
#[cfg(not(test))]
pub(crate) use inert::*;

#[cfg(not(test))]
mod inert {
    use super::Counter;

    /// Héritage de la portée courante : vide hors tests.
    #[derive(Clone)]
    pub(crate) struct Inherit;

    /// Garde vide.
    pub(crate) struct Guard;

    impl Inherit {
        #[inline]
        pub(crate) fn capture() -> Self {
            Inherit
        }
        #[inline]
        pub(crate) fn enter(self) -> Option<Guard> {
            None
        }
    }

    #[inline]
    pub(crate) fn add(_counter: Counter, _n: usize) {}
}

#[cfg(test)]
mod active {
    use super::Counter;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    const COUNTERS: usize = 5;

    /// Compteurs d'une portée.
    #[derive(Default)]
    pub(crate) struct Diag {
        counters: [AtomicUsize; COUNTERS],
        force_fresh: AtomicBool,
    }

    thread_local! {
        static CURRENT: RefCell<Option<Arc<Diag>>> = const { RefCell::new(None) };
    }

    /// Portée installée sur le fil courant ; restaure la précédente au `Drop`.
    pub(crate) struct Guard {
        diag: Arc<Diag>,
        previous: Option<Arc<Diag>>,
    }

    /// Installe une portée neuve sur le fil courant.
    pub(crate) fn scope() -> Guard {
        install(Arc::new(Diag::default()))
    }

    fn install(diag: Arc<Diag>) -> Guard {
        let previous = CURRENT.with(|c| c.replace(Some(Arc::clone(&diag))));
        Guard { diag, previous }
    }

    impl Guard {
        pub(crate) fn get(&self, counter: Counter) -> usize {
            self.diag.counters[counter as usize].load(Ordering::SeqCst)
        }
        pub(crate) fn set_force_fresh(&self, on: bool) {
            self.diag.force_fresh.store(on, Ordering::SeqCst);
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let previous = self.previous.take();
            CURRENT.with(|c| *c.borrow_mut() = previous);
        }
    }

    /// Ajoute `n` au compteur de la portée courante ; sans portée : rien.
    pub(crate) fn add(counter: Counter, n: usize) {
        CURRENT.with(|c| {
            if let Some(d) = c.borrow().as_ref() {
                d.counters[counter as usize].fetch_add(n, Ordering::SeqCst);
            }
        });
    }

    /// Valeur du compteur dans la portée courante (0 sans portée).
    pub(crate) fn current(counter: Counter) -> usize {
        CURRENT.with(|c| {
            c.borrow()
                .as_ref()
                .map_or(0, |d| d.counters[counter as usize].load(Ordering::SeqCst))
        })
    }

    /// Crochet `FORCE_FRESH` de la portée courante.
    pub(crate) fn force_fresh() -> bool {
        CURRENT.with(|c| {
            c.borrow()
                .as_ref()
                .is_some_and(|d| d.force_fresh.load(Ordering::SeqCst))
        })
    }

    /// Portée du fil appelant, à entrer dans un fil démarré.
    #[derive(Clone)]
    pub(crate) struct Inherit(Option<Arc<Diag>>);

    impl Inherit {
        pub(crate) fn capture() -> Self {
            Inherit(CURRENT.with(|c| c.borrow().clone()))
        }
        /// Installe la portée capturée sur le fil courant (`None` s'il n'y en avait pas).
        pub(crate) fn enter(self) -> Option<Guard> {
            self.0.map(install)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn portee_heritee_et_restauree() {
            add(Counter::HashThreads, 5); // sans portée : rien
            let d = scope();
            add(Counter::HashThreads, 2);
            let inh = Inherit::capture();
            std::thread::spawn(move || {
                let _g = inh.enter();
                add(Counter::HashThreads, 3);
            })
            .join()
            .unwrap();
            assert_eq!(d.get(Counter::HashThreads), 5);
            {
                let e = scope();
                add(Counter::HashThreads, 1);
                assert_eq!(e.get(Counter::HashThreads), 1);
            }
            add(Counter::HashThreads, 1);
            assert_eq!(d.get(Counter::HashThreads), 6);
        }
    }
}
