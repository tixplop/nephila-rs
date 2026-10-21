//! Type d'erreur unique, affichage sur stderr et codes de sortie.

use std::fmt;

/// Erreur de l'application ; tout `Err` remonté à `main` donne un code non nul.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Clé d'API refusée par le fournisseur (HTTP 401/403).
    InvalidKey(String),
    /// Erreur réseau, quota ou erreur serveur du fournisseur.
    Network(String),
    /// Délai d'appel dépassé (`timeout_secs`).
    Timeout(String),
    /// Toute autre erreur, avec un message destiné à l'utilisateur.
    Message(String),
    /// Réponse reçue du LLM jugée inexploitable (illisible, hors schéma) ; seule erreur
    /// qui justifie un nouvel essai (REQ-038). S'affiche comme `Message`.
    Format(String),
    /// Réponse que le fournisseur marque tronquée par sa limite de jetons ; jamais retentée, ni
    /// comptée ni remise à zéro par la politique d'échec (REQ-046). S'affiche comme `Message`.
    Truncated(String),
    /// Plan d'appels d'un fichier envoyé seul au-delà du plafond : aucun appel n'est envoyé.
    /// Ni comptée ni remise à zéro par la politique d'échec, jamais retentée (REQ-048).
    TooManyCalls { calls: usize, max: usize },
}

/// Libellé fixe d'une réponse tronquée (avertissement d'enrichissement, erreur de `query ask`,
/// traces) : seule source du texte.
pub const TRUNCATED: &str = "réponse tronquée par la limite de jetons";

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
            Error::InvalidKey(m)
            | Error::Network(m)
            | Error::Timeout(m)
            | Error::Message(m)
            | Error::Format(m)
            | Error::Truncated(m) => f.write_str(m),
            Error::TooManyCalls { calls, max } => write!(
                f,
                "trop d'appels d'enrichissement ({calls} nécessaires, au plus {max} par fichier), aucun envoyé ; excluez ce fichier avec exclude dans .nephila.toml"
            ),
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
