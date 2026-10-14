//! Type d'erreur unique, affichage sur stderr et codes de sortie.

use std::fmt;

/// Erreur de l'application ; tout `Err` remonté à `main` donne un code non nul.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Clé d'API refusée par le fournisseur (HTTP 401/403).
    InvalidKey(String),
    /// Erreur réseau, quota ou erreur serveur du fournisseur.
    Network(String),
    /// Toute autre erreur, avec un message destiné à l'utilisateur.
    Message(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Code de sortie d'une erreur d'exécution (l'usage clap sort avec 2).
pub const EXIT_ERROR: i32 = 1;

impl Error {
    pub fn exit_code(&self) -> i32 {
        EXIT_ERROR
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidKey(m) | Error::Network(m) | Error::Message(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Écrit `erreur: <message>` sur stderr.
pub fn report_error(e: &Error) {
    eprintln!("erreur: {e}");
}

/// Écrit `avertissement: <message>` sur stderr.
pub fn warn(msg: &str) {
    eprintln!("avertissement: {msg}");
}
