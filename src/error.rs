//! Type d'erreur unique, affichage sur stderr et codes de sortie.

use std::fmt;

/// Erreur de l'application ; tout `Err` remonté à `main` donne un code non nul.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Clé d'API refusée par le fournisseur (HTTP 401/403).
    InvalidKey(String),
    /// Requête refusée par le fournisseur (HTTP 400, 404, 413, 422 ; REQ-091, DEC-099) : jamais
    /// retentée, arrête l'envoi dès le premier appel. `message` est le texte E-19 complet,
    /// `trace` le détail fixe de la trace (sans rien recopier de la réponse), `code` le statut HTTP
    /// et `detail` l'`error.message` normalisé du fournisseur, s'il en a donné un (REQ-203).
    Rejected {
        message: String,
        trace: String,
        code: u16,
        detail: Option<String>,
    },
    /// Réponse HTTP 3xx du fournisseur (REQ-092, DEC-100) : jamais suivie ni retentée, arrête
    /// l'envoi dès le premier appel. Mêmes champs que `Rejected`.
    Redirected { message: String, trace: String },
    /// Erreur réseau, quota ou erreur serveur du fournisseur.
    Network(String),
    /// Échec définitif d'un appel sur HTTP 429 ou 529 (REQ-093, DEC-099) : compté comme une
    /// erreur réseau par la politique, mais la cause d'arrêt dit « fournisseur saturé ».
    Saturated(String),
    /// Délai d'appel dépassé (`timeout_secs`).
    Timeout(String),
    /// Toute autre erreur, avec un message destiné à l'utilisateur.
    Message(String),
    /// Réponse reçue du LLM jugée inexploitable (illisible, hors schéma) ; seule erreur
    /// qui justifie un nouvel essai (REQ-038). S'affiche comme `Message`.
    Format(String),
    /// Réponse que le fournisseur marque tronquée par sa limite de jetons ; jamais retentée, ni
    /// comptée ni remise à zéro par la politique d'échec (REQ-046). S'affiche comme `Message`.
    /// `partial` est le texte reçu avant la coupe (vide s'il manque), jamais affiché (REQ-232).
    Truncated { message: String, partial: String },
    /// Réponse que le fournisseur marque bloquée (refus, filtrage de contenu) ; jamais retentée, ni
    /// comptée ni remise à zéro par la politique d'échec (REQ-274). S'affiche comme `Message`.
    Refused(String),
    /// Plan d'appels d'un fichier envoyé seul au-delà du plafond : aucun appel n'est envoyé.
    /// Ni comptée ni remise à zéro par la politique d'échec, jamais retentée (REQ-048).
    TooManyCalls { calls: usize, max: usize },
    /// Plan d'appels d'un fichier envoyé seul au-delà du plafond d'appels de la commande
    /// (`max_calls`, A-11) : aucun appel n'est envoyé, aucun compte n'est pris.
    CallsOverCap { calls: usize, max: usize },
    /// Essai refusé faute de compte par le plafond d'appels de la commande (REQ-186) : erreur
    /// interne, sans issue pour le fichier ; jamais affichée ni écrite dans un avertissement.
    Capped,
    /// Requête mise de côté par le collecteur du mode lot (DEC-168) : erreur interne, sans issue
    /// pour le fichier ; jamais affichée ni écrite dans un avertissement.
    Deferred,
    /// Appel arrêté avant son premier envoi, pendant la pause partagée (REQ-254) : erreur
    /// interne, sans issue pour le fichier ; jamais affichée ni écrite dans un avertissement.
    Unsent,
    /// Requête du lot du fournisseur en échec (A-17) : `cause` est le texte de l'avertissement,
    /// `refused` le message d'un refus HTTP 400 ou `invalid_request_error`, lu par le repli de
    /// `structured_output`.
    BatchItem {
        cause: String,
        refused: Option<String>,
    },
    /// Fin d'une commande dont l'envoi a été arrêté par Ctrl-C (E-27) ; s'affiche comme `Message`,
    /// code de sortie 130.
    Interrupted(String),
}

/// Libellé fixe d'une réponse tronquée (avertissement d'enrichissement, erreur de `query ask`,
/// traces) : seule source du texte.
pub const TRUNCATED: &str = "réponse tronquée par la limite de jetons";

/// Libellé fixe d'une réponse bloquée par le modèle (A-24, E-37, cause d'A-23, traces) : seule
/// source du texte.
pub const BLOCKED: &str = "réponse bloquée par le modèle";

pub type Result<T> = std::result::Result<T, Error>;

/// Code de sortie d'une erreur d'exécution (l'usage clap sort avec 2).
pub const EXIT_ERROR: i32 = 1;

/// Erreur E-17 (écriture sur la sortie standard) : seule source du texte.
pub fn stdout_write(e: &std::io::Error) -> Error {
    Error::Message(format!("écriture sur stdout impossible : {e}"))
}

impl Error {
    /// Troncature sans texte récupéré (`partial` vide).
    pub fn truncated(message: impl AsRef<str>) -> Error {
        Error::Truncated {
            message: message.as_ref().to_string(),
            partial: String::new(),
        }
    }

    /// Variable d'environnement numérique mal formée (E-14) : seule source du texte.
    pub fn bad_integer_env(name: &str, value: &str) -> Error {
        Error::Message(format!("{name} : entier attendu, reçu « {value} »"))
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Interrupted(_) => crate::interrupt::EXIT_INTERRUPTED,
            _ => EXIT_ERROR,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidKey(m)
            | Error::Rejected { message: m, .. }
            | Error::Redirected { message: m, .. }
            | Error::Network(m)
            | Error::Saturated(m)
            | Error::Timeout(m)
            | Error::Message(m)
            | Error::Format(m)
            | Error::Refused(m)
            | Error::Truncated { message: m, .. }
            | Error::Interrupted(m) => f.write_str(m),
            Error::TooManyCalls { calls, max } => write!(
                f,
                "trop d'appels d'enrichissement ({calls} nécessaires, au plus {max} par fichier), aucun envoyé ; excluez ce fichier avec exclude dans .nephila.toml"
            ),
            Error::CallsOverCap { calls, max } => write!(
                f,
                "trop d'appels d'enrichissement ({calls} nécessaires, au plus {max} par commande), aucun envoyé ; augmentez max_calls ou excluez ce fichier avec exclude dans .nephila.toml"
            ),
            // Jamais affichée : le refus n'a pas d'issue (voir `Error::Capped`).
            Error::Capped => f.write_str("plafond d'appels atteint"),
            // Jamais affichée : la requête est mise de côté (voir `Error::Deferred`).
            Error::Deferred => f.write_str("requête mise de côté"),
            // Jamais affichée : l'appel n'a rien envoyé (voir `Error::Unsent`).
            Error::Unsent => f.write_str("aucun envoi"),
            Error::BatchItem { cause, .. } => write!(
                f,
                "requête du lot du fournisseur en échec ({cause}) ; relancez nephila update --batch"
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
    eprintln!("avertissement: {}", crate::escape::data(msg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_displays_its_message_and_exits_130() {
        let e = Error::Interrupted("envoi interrompu : Ctrl-C reçu".into());
        assert_eq!(e.to_string(), "envoi interrompu : Ctrl-C reçu");
        assert_eq!(e.exit_code(), 130);
        assert_eq!(Error::Message("x".into()).exit_code(), EXIT_ERROR);
        assert_eq!(Error::Capped.exit_code(), 1);
    }

    #[test]
    fn req214_batch_item_display() {
        let e = Error::BatchItem {
            cause: "expirée".into(),
            refused: None,
        };
        assert_eq!(
            e.to_string(),
            "requête du lot du fournisseur en échec (expirée) ; relancez nephila update --batch"
        );
        assert_eq!(e.exit_code(), 1);
    }
}
