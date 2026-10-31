//! Nouveaux essais d'un appel LLM dont la réponse est non conforme (REQ-038, DEC-046), avec une
//! note de correction selon la catégorie de l'erreur précédente (REQ-177, DEC-143).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

/// Note de `FormatFault::Absent`.
pub const NOTE_ABSENT: &str = "Correction : ta réponse précédente ne contenait aucun objet JSON. Réponds uniquement par un objet JSON au format demandé, sans texte autour.";
/// Note de `FormatFault::Several`.
pub const NOTE_SEVERAL: &str = "Correction : ta réponse précédente contenait plusieurs objets JSON différents. Réponds par un seul objet JSON au format demandé, sans texte autour.";
/// Note de `FormatFault::Unreadable`.
pub const NOTE_UNREADABLE: &str = "Correction : ta réponse précédente contenait un JSON illisible (syntaxe invalide ou objet incomplet). Réponds par un seul objet JSON complet et valide, au format demandé.";
/// Note de `FormatFault::Version`.
pub const NOTE_VERSION: &str = "Correction : dans ta réponse précédente, la valeur de \"version\" n'était pas celle du format demandé. Recopie la version donnée par le schéma.";
/// Note de `FormatFault::UnknownField`.
pub const NOTE_UNKNOWN_FIELD: &str = "Correction : ta réponse précédente contenait un champ hors du format demandé. N'utilise que les champs du format demandé, sans en ajouter ni en renommer.";
/// Note de `FormatFault::MissingField`.
pub const NOTE_MISSING_FIELD: &str = "Correction : il manquait un champ obligatoire du format demandé dans ta réponse précédente. Donne tous les champs obligatoires, avec une liste vide s'il n'y a rien à y mettre.";
/// Note de `FormatFault::Shape`.
pub const NOTE_SHAPE: &str = "Correction : dans ta réponse précédente, un champ n'avait pas le type ou la valeur demandés (par exemple une chaîne à la place d'un booléen ou d'une liste). Respecte exactement le format demandé.";

const PREFIX: &str = "réponse LLM non conforme";
const PREFIX_SCHEMA: &str = "réponse LLM non conforme au schéma : ";

/// Catégorie d'une erreur de format, lue sur son message (REQ-177, DEC-143).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatFault {
    Absent,
    Several,
    Unreadable,
    Version,
    UnknownField,
    MissingField,
    Shape,
}

impl FormatFault {
    /// Les sept catégories, dans l'ordre de REQ-177.
    pub const ALL: [FormatFault; 7] = [
        FormatFault::Absent,
        FormatFault::Several,
        FormatFault::Unreadable,
        FormatFault::Version,
        FormatFault::UnknownField,
        FormatFault::MissingField,
        FormatFault::Shape,
    ];

    /// Catégorie d'une erreur ; `None` hors `Error::Format` et pour les erreurs d'enveloppe.
    pub fn of(e: &Error) -> Option<FormatFault> {
        let Error::Format(m) = e else {
            return None;
        };
        if m.starts_with(&format!("{PREFIX} : aucun objet JSON dans le texte")) {
            return Some(FormatFault::Absent);
        }
        if m.starts_with(&format!("{PREFIX} : plusieurs objets JSON différents")) {
            return Some(FormatFault::Several);
        }
        let d = m.strip_prefix(PREFIX_SCHEMA)?;
        let starts = |ps: &[&str]| ps.iter().any(|p| d.starts_with(p));
        Some(if d.starts_with("trailing characters") {
            FormatFault::Several
        } else if d.starts_with("version ") && d.contains(" non prise en charge") {
            FormatFault::Version
        } else if d.starts_with("unknown field ") {
            FormatFault::UnknownField
        } else if d.starts_with("missing field ") || d.starts_with("« answer » manquant") {
            FormatFault::MissingField
        } else if starts(&[
            "invalid type: ",
            "invalid value: ",
            "invalid length ",
            "duplicate field ",
            "unknown variant ",
            "« found » faux avec une réponse",
        ]) {
            FormatFault::Shape
        } else {
            FormatFault::Unreadable
        })
    }

    /// Note de correction de la catégorie.
    pub fn note(self) -> &'static str {
        match self {
            FormatFault::Absent => NOTE_ABSENT,
            FormatFault::Several => NOTE_SEVERAL,
            FormatFault::Unreadable => NOTE_UNREADABLE,
            FormatFault::Version => NOTE_VERSION,
            FormatFault::UnknownField => NOTE_UNKNOWN_FIELD,
            FormatFault::MissingField => NOTE_MISSING_FIELD,
            FormatFault::Shape => NOTE_SHAPE,
        }
    }
}

/// Message utilisateur d'un nouvel essai : `user` seul sans catégorie, sinon `user`, `\n\n` et
/// la note (REQ-177).
pub fn with_note(user: String, fault: Option<FormatFault>) -> String {
    match fault {
        None => user,
        Some(f) => format!("{user}\n\n{}", f.note()),
    }
}

/// Appelle `call` (arguments = numéro d'essai, de 1 à `retries + 1`, et catégorie de l'erreur de
/// l'essai précédent, REQ-177 et DEC-143) tant qu'il rend `Err(Format)`, qu'il reste des essais
/// et que `stop` est faux. Un `Ok` ou une autre erreur termine aussitôt. Une erreur de format
/// finale reçoit, si `retries >= 1`, le suffixe ` (<k> essais sur <t>)`.
pub fn with_format_retries<T>(
    retries: u32,
    stop: &AtomicBool,
    mut call: impl FnMut(u32, Option<FormatFault>) -> Result<T>,
) -> Result<T> {
    let total = retries.saturating_add(1);
    let mut attempt = 1;
    let mut fault = None;
    loop {
        match call(attempt, fault) {
            Err(Error::Format(m)) => {
                if attempt >= total || stop.load(Ordering::SeqCst) {
                    return Err(if retries == 0 {
                        Error::Format(m)
                    } else {
                        Error::Format(format!("{m} ({attempt} essais sur {total})"))
                    });
                }
                fault = FormatFault::of(&Error::Format(m));
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
        let r = with_format_retries(retries, stop, |n, _| {
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
        let r: Result<()> = with_format_retries(2, &AtomicBool::new(false), |n, _| {
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
        let r: Result<()> = with_format_retries(2, &stop, |_, _| {
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
        let r: Result<()> = with_format_retries(2, &AtomicBool::new(false), |_, _| {
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
        let r: Result<()> = with_format_retries(5, &AtomicBool::new(false), |n, _| {
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
        let r: Result<()> = with_format_retries(0, &AtomicBool::new(false), |_, _| {
            calls += 1;
            Err(truncated())
        });
        assert_eq!((calls, r), (1, Err(truncated())));
    }

    // ---- REQ-177 : catégorie d'erreur et notes ----

    mod fault {
        use super::super::*;
        use crate::llm::enrich::{parse_lot, Enrichment};
        use crate::llm::http::{parse_body, parse_reply};
        use crate::llm::AskReply;

        const EXTRACTION: &str =
            r#"{"version":2,"summaries":[{"node":"type:A","summary":"Le A."}],"concepts":[]}"#;

        fn nodes() -> Vec<String> {
            vec!["type:A".to_string()]
        }

        fn enrich_err(text: &str) -> Error {
            parse_reply(text, "version", |j| {
                Enrichment::parse(j, "A.java", &nodes())
            })
            .map(|_| ())
            .unwrap_err()
        }

        fn parse_err(json: &str) -> Error {
            Enrichment::parse(json, "A.java", &nodes())
                .map(|_| ())
                .unwrap_err()
        }

        fn ask_err(json: &str) -> Error {
            AskReply::parse(json).map(|_| ()).unwrap_err()
        }

        fn is(e: &Error, f: Option<FormatFault>) {
            assert_eq!(FormatFault::of(e), f, "{e}");
            if let Error::Format(m) = e {
                let suffixed = Error::Format(format!("{m} (3 essais sur 3)"));
                assert_eq!(FormatFault::of(&suffixed), f, "{suffixed}");
            }
        }

        #[test]
        fn req177_ac1_absent() {
            is(
                &enrich_err("désolé, pas de JSON"),
                Some(FormatFault::Absent),
            );
        }

        #[test]
        fn req177_ac1_several() {
            let other = EXTRACTION.replace("Le A.", "Autre.");
            let e = enrich_err(&format!("{EXTRACTION}\n{other}"));
            is(&e, Some(FormatFault::Several));
            let e =
                Error::Format("réponse LLM non conforme : plusieurs objets JSON différents".into());
            is(&e, Some(FormatFault::Several));
        }

        #[test]
        fn req177_ac1_unreadable() {
            is(
                &parse_err(r#"{"version":2,"summaries":["#),
                Some(FormatFault::Unreadable),
            );
            is(&ask_err(r#"{"found":tru}"#), Some(FormatFault::Unreadable));
        }

        #[test]
        fn req177_ac1_version() {
            is(
                &parse_err(r#"{"version":1,"summaries":[],"concepts":[]}"#),
                Some(FormatFault::Version),
            );
            let e = parse_lot(r#"{"version":2,"files":[]}"#, &[])
                .map(|_| ())
                .unwrap_err();
            is(&e, Some(FormatFault::Version));
        }

        #[test]
        fn req177_ac1_unknown_field() {
            is(
                &parse_err(r#"{"version":2,"summaries":[],"concepts":[],"extra":1}"#),
                Some(FormatFault::UnknownField),
            );
            let wrapped = format!(r#"{{"result":{EXTRACTION}}}"#);
            is(&enrich_err(&wrapped), Some(FormatFault::UnknownField));
            let e = parse_reply(EXTRACTION, "version", |j| parse_lot(j, &[])).map(|_| ());
            is(&e.unwrap_err(), Some(FormatFault::UnknownField));
        }

        #[test]
        fn req177_ac1_missing_field() {
            is(
                &parse_err(r#"{"version":2,"summaries":[]}"#),
                Some(FormatFault::MissingField),
            );
            is(
                &enrich_err(r#"{"summaries":[]}"#),
                Some(FormatFault::MissingField),
            );
            is(
                &ask_err(r#"{"found":true}"#),
                Some(FormatFault::MissingField),
            );
        }

        #[test]
        fn req177_ac1_shape() {
            is(&ask_err(r#"{"found":"oui"}"#), Some(FormatFault::Shape));
            is(
                &ask_err(r#"{"found":false,"answer":"x"}"#),
                Some(FormatFault::Shape),
            );
            is(
                &parse_err(r#"{"version":"2","summaries":[],"concepts":[]}"#),
                Some(FormatFault::Shape),
            );
        }

        #[test]
        fn req177_ac1_no_category() {
            is(
                &Error::Format("réponse de openai sans message".into()),
                None,
            );
            is(
                &Error::Format("réponse de anthropic sans bloc texte".into()),
                None,
            );
            is(&parse_body("openai", "pas du json").unwrap_err(), None);
            is(&Error::Message("x".into()), None);
            is(&Error::Network("x".into()), None);
            is(&Error::Truncated("x".into()), None);
        }

        #[test]
        fn req177_ac2_notes_are_exact() {
            let expected = [
                (NOTE_ABSENT, "Correction : ta réponse précédente ne contenait aucun objet JSON. Réponds uniquement par un objet JSON au format demandé, sans texte autour."),
                (NOTE_SEVERAL, "Correction : ta réponse précédente contenait plusieurs objets JSON différents. Réponds par un seul objet JSON au format demandé, sans texte autour."),
                (NOTE_UNREADABLE, "Correction : ta réponse précédente contenait un JSON illisible (syntaxe invalide ou objet incomplet). Réponds par un seul objet JSON complet et valide, au format demandé."),
                (NOTE_VERSION, "Correction : dans ta réponse précédente, la valeur de \"version\" n'était pas celle du format demandé. Recopie la version donnée par le schéma."),
                (NOTE_UNKNOWN_FIELD, "Correction : ta réponse précédente contenait un champ hors du format demandé. N'utilise que les champs du format demandé, sans en ajouter ni en renommer."),
                (NOTE_MISSING_FIELD, "Correction : il manquait un champ obligatoire du format demandé dans ta réponse précédente. Donne tous les champs obligatoires, avec une liste vide s'il n'y a rien à y mettre."),
                (NOTE_SHAPE, "Correction : dans ta réponse précédente, un champ n'avait pas le type ou la valeur demandés (par exemple une chaîne à la place d'un booléen ou d'une liste). Respecte exactement le format demandé."),
            ];
            assert_eq!(FormatFault::ALL.len(), 7);
            for (i, f) in FormatFault::ALL.iter().enumerate() {
                assert_eq!(f.note(), expected[i].0);
                assert_eq!(expected[i].0, expected[i].1);
                let n = f.note();
                assert!(n.starts_with("Correction : "));
                for bad in ["\n", "{", "}", "extrait", "morceau", "Concepts :"] {
                    assert!(!n.contains(bad), "{n} contient {bad:?}");
                }
                for g in &FormatFault::ALL[i + 1..] {
                    assert_ne!(f, g);
                    assert_ne!(f.note(), g.note());
                }
            }
            assert_eq!(FormatFault::ALL[0], FormatFault::Absent);
            assert_eq!(FormatFault::ALL[6], FormatFault::Shape);
        }

        #[test]
        fn req177_ac2_with_note() {
            for u in ["", "message", "message\n\n"] {
                assert_eq!(with_note(u.to_string(), None), u);
                assert_eq!(
                    with_note(u.to_string(), Some(FormatFault::Shape)),
                    format!("{u}\n\n{NOTE_SHAPE}")
                );
            }
        }

        fn seen(errors: Vec<Error>) -> Vec<(u32, Option<FormatFault>)> {
            let mut errors = errors.into_iter();
            let mut log = Vec::new();
            let _ = with_format_retries(5, &AtomicBool::new(false), |n, f| {
                log.push((n, f));
                match errors.next() {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            });
            log
        }

        #[test]
        fn req177_ac3_closure_receives_previous_fault() {
            let fmt = |m: &str| Error::Format(m.into());
            let absent = fmt("réponse LLM non conforme : aucun objet JSON dans le texte");
            let unknown = fmt("réponse LLM non conforme au schéma : unknown field `x`");
            assert_eq!(
                seen(vec![absent, unknown]),
                vec![
                    (1, None),
                    (2, Some(FormatFault::Absent)),
                    (3, Some(FormatFault::UnknownField))
                ]
            );
            assert_eq!(
                seen(vec![fmt("réponse de openai sans message")]),
                vec![(1, None), (2, None)]
            );
            let unreadable = fmt("réponse LLM non conforme au schéma : EOF while parsing a list");
            assert_eq!(
                seen(vec![unreadable, fmt("réponse de openai sans message")]),
                vec![(1, None), (2, Some(FormatFault::Unreadable)), (3, None)]
            );
        }
    }
}
