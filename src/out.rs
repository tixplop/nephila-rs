//! Écrivain unique de la sortie standard (REQ-086, DEC-095).

use std::io::Write;

use crate::error::{self, Result};

/// Sortie standard d'une commande : toute ligne de stdout passe par `writeln!(out, …)?`.
pub struct Out {
    inner: Box<dyn Write>,
}

impl Out {
    /// La sortie standard du processus (tamponnée par ligne, non verrouillée).
    pub fn stdout() -> Out {
        Out::new(Box::new(std::io::stdout()))
    }

    pub fn new(inner: Box<dyn Write>) -> Out {
        Out { inner }
    }

    /// Écrit les arguments formatés ; l'erreur d'écriture devient l'erreur E-17.
    pub fn write_fmt(&mut self, args: std::fmt::Arguments) -> Result<()> {
        self.inner
            .write_fmt(args)
            .map_err(|e| error::stdout_write(&e))
    }

    pub fn flush(&mut self) -> Result<()> {
        self.inner.flush().map_err(|e| error::stdout_write(&e))
    }
}
