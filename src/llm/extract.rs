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
            });
        }
        let mut references = Vec::with_capacity(raw.references.len());
        for r in raw.references {
            let (skind, sname) = parse_node_id(&r.source).map_err(invalid)?;
            if sname.is_empty() {
                return Err(invalid(format!("source vide : « {} »", r.source)));
            }
            let kind = EdgeKind::parse(&r.kind).map_err(invalid)?;
            if !NodeKind::ALL.iter().any(|&to| kind.allows(skind, to)) {
                return Err(invalid(format!(
                    "un lien {kind} ne peut pas partir d'un nœud {skind}"
                )));
            }
            if r.target.trim().is_empty() {
                return Err(invalid("cible de référence vide"));
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
    format!(
        "Tu analyses un fichier d'un dépôt (code Java/Kotlin/Groovy/Scala, build Maven/Gradle/Ivy, \
         ou Markdown) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Schéma (version {SCHEMA_VERSION}) : {{\"version\": {SCHEMA_VERSION}, \"context\": \"lignes `package a.b` et `import a.b.C` (optionnel)\", \
         \"declares\": [{{\"kind\": <genre>, \"name\": \"nom.qualifié\", \"subkind\": <sous-genre, type seulement>, \
         \"summary\": \"une phrase (optionnel)\"}}], \
         \"references\": [{{\"source\": \"genre:nom.qualifié\", \"kind\": <lien>, \"target\": \"nom tel qu'écrit\"}}]}}.\n\
         Genres de nœuds : {}. Sous-genres : {}. Genres de liens : {}.\n\
         Les méthodes sont qualifiées par leur type (OrderService.place). Les cibles de références \
         sont des noms, pas des identifiants.",
        kinds.join(", "),
        subs.join(", "),
        edges.join(", ")
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
