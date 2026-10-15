//! Prompt d'extraction et schéma JSON versionné (DEC-010, REQ-006).

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{node_id, parse_node_id, EdgeKind, NodeKind, SubKind};

/// Version du schéma de réponse.
pub const SCHEMA_VERSION: u32 = 1;

/// Nœud déclaré par le fichier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub kind: NodeKind,
    pub qualified: String,
    pub subkind: Option<SubKind>,
    pub summary: Option<String>,
    /// Nom court fixé par l'extracteur (DEC-024) ; `None` = dérivé du nom qualifié.
    pub short: Option<String>,
}

impl Declared {
    pub fn id(&self) -> String {
        node_id(self.kind, &self.qualified)
    }
}

/// Référence par nom depuis un nœud déclaré.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Identifiant `genre:nom.qualifié` de la source.
    pub source: String,
    pub kind: EdgeKind,
    /// Nom cible tel qu'écrit dans le fichier.
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Extraction {
    /// Contexte du fichier (package, imports), texte libre.
    pub context: Option<String>,
    pub declares: Vec<Declared>,
    pub references: Vec<Reference>,
    /// Références écartées (genre de lien inconnu ou non permis depuis la source), décrites.
    pub rejected: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtraction {
    version: u32,
    context: Option<String>,
    declares: Vec<RawDeclared>,
    references: Vec<RawReference>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeclared {
    kind: String,
    name: String,
    subkind: Option<String>,
    summary: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReference {
    source: String,
    kind: String,
    target: String,
}

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::Message(format!("réponse LLM non conforme au schéma : {msg}"))
}

impl Extraction {
    /// Analyse et valide strictement une réponse JSON.
    pub fn parse(json: &str) -> Result<Extraction> {
        let raw: RawExtraction = serde_json::from_str(json).map_err(invalid)?;
        if raw.version != SCHEMA_VERSION {
            return Err(invalid(format!(
                "version {} non prise en charge (attendu : {SCHEMA_VERSION})",
                raw.version
            )));
        }
        let mut declares = Vec::with_capacity(raw.declares.len());
        for d in raw.declares {
            let kind = NodeKind::parse(&d.kind).map_err(invalid)?;
            if d.name.trim().is_empty() {
                return Err(invalid("nom de nœud vide"));
            }
            let subkind = match d.subkind {
                None => None,
                Some(s) => {
                    if kind != NodeKind::Type {
                        return Err(invalid(format!(
                            "sous-genre « {s} » permis seulement pour un nœud de genre type"
                        )));
                    }
                    Some(SubKind::parse(&s).map_err(invalid)?)
                }
            };
            declares.push(Declared {
                kind,
                qualified: d.name,
                subkind,
                summary: d.summary,
                short: None,
            });
        }
        let mut references = Vec::with_capacity(raw.references.len());
        let mut rejected = Vec::new();
        for r in raw.references {
            let (skind, sname) = parse_node_id(&r.source).map_err(invalid)?;
            if sname.is_empty() {
                return Err(invalid(format!("source vide : « {} »", r.source)));
            }
            if r.target.trim().is_empty() {
                return Err(invalid("cible de référence vide"));
            }
            let Ok(kind) = EdgeKind::parse(&r.kind) else {
                rejected.push(format!(
                    "{} {} {} (genre de lien inconnu)",
                    r.source, r.kind, r.target
                ));
                continue;
            };
            if !NodeKind::ALL.iter().any(|&to| kind.allows(skind, to)) {
                rejected.push(format!(
                    "{} {} {} (un lien {kind} ne peut pas partir d'un nœud {skind})",
                    r.source, r.kind, r.target
                ));
                continue;
            }
            references.push(Reference {
                source: r.source,
                kind,
                target: r.target,
            });
        }
        Ok(Extraction {
            context: raw.context,
            declares,
            references,
            rejected,
        })
    }
}

/// Réponse à `query ask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskReply {
    Found { answer: String, nodes: Vec<String> },
    NotFound,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsk {
    found: bool,
    answer: Option<String>,
    nodes: Option<Vec<String>>,
}

impl AskReply {
    pub fn parse(json: &str) -> Result<AskReply> {
        let raw: RawAsk = serde_json::from_str(json).map_err(invalid)?;
        if !raw.found {
            if raw.answer.is_some() || raw.nodes.is_some() {
                return Err(invalid("« found » faux avec une réponse"));
            }
            return Ok(AskReply::NotFound);
        }
        let answer = raw
            .answer
            .filter(|a| !a.trim().is_empty())
            .ok_or_else(|| invalid("« answer » manquant"))?;
        Ok(AskReply::Found {
            answer,
            nodes: raw.nodes.unwrap_or_default(),
        })
    }
}

/// Prompt système d'extraction (identique pour tous les fournisseurs).
pub fn extraction_system_prompt() -> String {
    let kinds: Vec<&str> = NodeKind::ALL.iter().map(|k| k.as_str()).collect();
    let edges: Vec<&str> = EdgeKind::ALL.iter().map(|k| k.as_str()).collect();
    let subs: Vec<&str> = SubKind::ALL.iter().map(|k| k.as_str()).collect();
    let rules: Vec<String> = EdgeKind::ALL
        .iter()
        .map(|&e| {
            let from: Vec<&str> = NodeKind::ALL
                .iter()
                .filter(|&&f| NodeKind::ALL.iter().any(|&t| e.allows(f, t)))
                .map(|k| k.as_str())
                .collect();
            format!("{} : {}", e.as_str(), from.join("|"))
        })
        .collect();
    format!(
        "Tu analyses un fichier d'un dépôt (code Java/Kotlin/Groovy/Scala, build Maven/Gradle/Ivy, \
         ou Markdown) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Schéma (version {SCHEMA_VERSION}) : {{\"version\": {SCHEMA_VERSION}, \"context\": \"lignes `package a.b` et `import a.b.C` (optionnel)\", \
         \"declares\": [{{\"kind\": <genre>, \"name\": \"nom.qualifié\", \"subkind\": <sous-genre, type seulement>, \
         \"summary\": \"une phrase (optionnel)\"}}], \
         \"references\": [{{\"source\": \"genre:nom.qualifié\", \"kind\": <lien>, \"target\": \"nom tel qu'écrit\"}}]}}.\n\
         Genres de nœuds : {}. Sous-genres : {}. Genres de liens : {}.\n\
         Un lien ne peut partir que de certains genres de nœuds (lien : genres de la source) : {}. \
         Une référence d'un autre genre de lien ou depuis un autre genre de nœud est écartée.\n\
         Les méthodes sont qualifiées par leur type (OrderService.place). Les cibles de références \
         sont des noms, pas des identifiants.",
        kinds.join(", "),
        subs.join(", "),
        edges.join(", "),
        rules.join(" ; ")
    )
}

/// Message utilisateur d'extraction pour un morceau de fichier.
pub fn extraction_user_prompt(file: &str, chunk: usize, text: &str) -> String {
    format!("Fichier : {file} (morceau {chunk})\n\n{text}")
}

/// Prompt système de `query ask`.
pub fn ask_system_prompt() -> String {
    "Tu réponds à une question sur un dépôt à partir du seul contexte de graphe fourni. \
     Réponds par un objet JSON : {\"found\": true, \"answer\": \"...\", \"nodes\": [\"identifiants cités\"]} \
     ou {\"found\": false} si le contexte ne permet pas de répondre."
        .to_string()
}

pub fn ask_user_prompt(question: &str, context: &str) -> String {
    format!("Question : {question}\n\nContexte :\n{context}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK_DECL: &str = r#"{"kind":"type","name":"a.A","subkind":"class"}"#;

    fn parse_refs(refs: &str) -> Result<Extraction> {
        Extraction::parse(&format!(
            r#"{{"version":1,"declares":[{OK_DECL}],"references":[{refs}]}}"#
        ))
    }

    #[test]
    fn req006_ac8_disallowed_and_unknown_edge_kinds_are_rejected_not_fatal() {
        let x = parse_refs(
            r#"{"source":"type:a.A","kind":"imports","target":"B"},
               {"source":"type:a.A","kind":"calls","target":"B.f"},
               {"source":"type:a.A","kind":"uses","target":"B"}"#,
        )
        .unwrap();
        assert_eq!(x.declares.len(), 1);
        assert_eq!(x.references.len(), 1);
        assert_eq!(x.references[0].target, "B");
        assert_eq!(x.rejected.len(), 2);
        assert!(x.rejected[0].contains("calls"), "{:?}", x.rejected);
        assert!(x.rejected[1].contains("uses"), "{:?}", x.rejected);
    }

    #[test]
    fn req006_ac8_other_defects_stay_non_conforming() {
        for bad in [
            r#"{"source":"nimporte","kind":"imports","target":"B"}"#,
            r#"{"source":"type:a.A","kind":"imports","target":" "}"#,
            r#"{"source":"type:","kind":"imports","target":"B"}"#,
        ] {
            assert!(parse_refs(bad).is_err(), "{bad}");
        }
        assert!(Extraction::parse(
            r#"{"version":1,"declares":[{"kind":"bogus","name":"X"}],"references":[]}"#
        )
        .is_err());
        assert!(Extraction::parse(r#"{"version":2,"declares":[],"references":[]}"#).is_err());
    }

    #[test]
    fn req006_ac8_system_prompt_lists_allowed_sources_per_edge_kind() {
        let p = extraction_system_prompt();
        assert!(p.contains("calls : method"), "{p}");
        assert!(p.contains("mentions : section"), "{p}");
        assert!(p.contains("depends_on : module"), "{p}");
    }
}
