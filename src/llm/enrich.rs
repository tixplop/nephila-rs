//! Prompt d'enrichissement et schéma JSON version 2 (DEC-019, REQ-024).

use std::collections::HashSet;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Version du schéma de réponse d'enrichissement.
pub const ENRICH_SCHEMA_VERSION: u32 = 2;

/// Concept proposé pour un document Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concept {
    /// Nom normalisé (minuscules, espaces réduits) = nom qualifié du nœud concept.
    pub name: String,
    pub summary: Option<String>,
    /// Identifiants des sections du fichier qui mentionnent le concept.
    pub sections: Vec<String>,
}

/// Résultat filtré de l'enrichissement d'un morceau (ou d'un fichier après fusion).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Enrichment {
    /// Couples (identifiant de nœud, résumé), nœuds de la liste fournie uniquement.
    pub summaries: Vec<(String, String)>,
    pub concepts: Vec<Concept>,
    /// Éléments écartés (hors liste, concept hors Markdown…), décrits.
    pub rejected: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnrichment {
    version: u32,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSummary {
    node: String,
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConcept {
    name: String,
    summary: Option<String>,
    sections: Vec<String>,
}

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::Message(format!("réponse LLM non conforme au schéma : {msg}"))
}

/// Minuscules, espaces réduits à un seul.
pub fn normalize_concept_name(name: &str) -> String {
    name.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Vrai si le chemin désigne un fichier Markdown.
pub fn is_markdown(file: &str) -> bool {
    std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

impl Enrichment {
    /// Analyse strictement `json` puis filtre selon la liste de nœuds du fichier.
    pub fn parse(json: &str, file: &str, node_ids: &[String]) -> Result<Enrichment> {
        let raw: RawEnrichment = serde_json::from_str(json).map_err(invalid)?;
        if raw.version != ENRICH_SCHEMA_VERSION {
            return Err(invalid(format!(
                "version {} non prise en charge (attendu : {ENRICH_SCHEMA_VERSION})",
                raw.version
            )));
        }
        let known: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
        let mut out = Enrichment::default();

        for s in raw.summaries {
            if s.node.trim().is_empty() {
                out.rejected.push("résumé sans identifiant de nœud".into());
                continue;
            }
            if !known.contains(s.node.as_str()) {
                out.rejected
                    .push(format!("résumé pour {} (nœud hors liste)", s.node));
            } else if !s.summary.trim().is_empty() {
                out.summaries.push((s.node, s.summary.trim().to_string()));
            }
        }

        let markdown = is_markdown(file);
        for c in raw.concepts {
            let name = normalize_concept_name(&c.name);
            if name.is_empty() {
                out.rejected.push("concept sans nom".into());
                continue;
            }
            if !markdown {
                out.rejected
                    .push(format!("concept {name} (fichier non Markdown)"));
                continue;
            }
            let mut sections: Vec<String> = Vec::new();
            let mut dropped = false;
            for s in c.sections {
                if !(s.starts_with("section:") && known.contains(s.as_str())) {
                    dropped = true;
                } else if !sections.contains(&s) {
                    sections.push(s);
                }
            }
            if sections.is_empty() {
                out.rejected
                    .push(format!("concept {name} (aucune section du fichier citée)"));
                continue;
            }
            if dropped {
                out.rejected
                    .push(format!("concept {name} : sections hors liste écartées"));
            }
            out.concepts.push(Concept {
                name,
                summary: c.summary.filter(|s| !s.trim().is_empty()),
                sections,
            });
        }
        Ok(out)
    }
}

/// Prompt système d'enrichissement (identique pour tous les fournisseurs).
pub fn enrichment_system_prompt() -> String {
    format!(
        "Tu enrichis le graphe d'un dépôt à partir d'un fichier (code Java/Kotlin/Groovy/Scala, \
         build Maven/Gradle/Ivy, ou Markdown) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Schéma (version {ENRICH_SCHEMA_VERSION}) : {{\"version\": {ENRICH_SCHEMA_VERSION}, \
         \"summaries\": [{{\"node\": \"<identifiant de la liste>\", \"summary\": \"une phrase\"}}], \
         \"concepts\": [{{\"name\": \"nom du concept\", \"summary\": \"une phrase (optionnel)\", \
         \"sections\": [\"<identifiant de section de la liste>\"]}}]}}.\n\
         Les résumés ne portent que sur les nœuds de la liste fournie. Les concepts ne concernent \
         que les fichiers Markdown et ne citent que des sections de la liste. Tu ne crées, ne \
         renomme ni ne supprimes aucun nœud et tu ne produis aucun autre lien. \
         Sans rien à signaler, réponds {{\"version\": {ENRICH_SCHEMA_VERSION}, \"summaries\": [], \"concepts\": []}}."
    )
}

/// Message utilisateur d'enrichissement : liste des nœuds puis contenu du morceau.
pub fn enrichment_user_prompt(file: &str, chunk: usize, text: &str, node_ids: &[String]) -> String {
    let list: String = node_ids.iter().map(|n| format!("- {n}\n")).collect();
    format!("Fichier : {file} (morceau {chunk})\n\nNœuds du fichier :\n{list}\nContenu :\n{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn req024_ac4_filters_out_of_list_items() {
        let nodes = ids(&["type:com.acme.A", "section:docs/g.md#intro"]);
        let json = r#"{"version":2,
            "summaries":[{"node":"type:com.acme.A","summary":" Fait A. "},
                         {"node":"type:com.acme.Inexistant","summary":"x"}],
            "concepts":[{"name":"Facturation  Client","summary":"s","sections":["section:docs/g.md#intro"]},
                        {"name":"Autre","sections":["section:other.md#x"]}]}"#;
        let e = Enrichment::parse(json, "docs/g.md", &nodes).unwrap();
        assert_eq!(
            e.summaries,
            vec![("type:com.acme.A".into(), "Fait A.".into())]
        );
        assert_eq!(e.concepts.len(), 1);
        assert_eq!(e.concepts[0].name, "facturation client");
        assert_eq!(e.rejected.len(), 2, "{:?}", e.rejected);
    }

    #[test]
    fn req024_ac4_concept_in_non_markdown_is_rejected() {
        let nodes = ids(&["type:A"]);
        let json =
            r#"{"version":2,"summaries":[],"concepts":[{"name":"X","sections":["type:A"]}]}"#;
        let e = Enrichment::parse(json, "A.java", &nodes).unwrap();
        assert!(e.concepts.is_empty());
        assert_eq!(e.rejected.len(), 1);
    }

    #[test]
    fn req024_ac5_non_conforming_responses() {
        let n = ids(&[]);
        for bad in [
            "pas du json",
            r#"{"version":1,"summaries":[],"concepts":[]}"#,
            r#"{"version":2,"summaries":[],"concepts":[],"extra":1}"#,
            r#"{"version":2,"summaries":[{"node":"a","summary":"b","x":1}],"concepts":[]}"#,
            r#"{"version":2,"summaries":[]}"#,
        ] {
            assert!(Enrichment::parse(bad, "a.md", &n).is_err(), "{bad}");
        }
        let ok =
            Enrichment::parse(r#"{"version":2,"summaries":[],"concepts":[]}"#, "a.md", &n).unwrap();
        assert_eq!(ok, Enrichment::default());
    }

    #[test]
    fn req024_ac4_empty_id_or_name_filtered_rest_applied() {
        let n = ids(&["type:A", "section:a.md#s"]);
        let json = r#"{"version":2,
            "summaries":[{"node":" ","summary":"x"},{"node":"type:A","summary":"ok"}],
            "concepts":[{"name":"  ","sections":["section:a.md#s"]},
                        {"name":"Bon","sections":["section:a.md#s"]}]}"#;
        let e = Enrichment::parse(json, "a.md", &n).unwrap();
        assert_eq!(e.summaries, vec![("type:A".into(), "ok".into())]);
        assert_eq!(e.concepts.len(), 1);
        assert_eq!(e.rejected.len(), 2, "{:?}", e.rejected);
    }

    #[test]
    fn req024_ac4_concept_section_needs_section_prefix() {
        let n = ids(&["type:A"]);
        let json =
            r#"{"version":2,"summaries":[],"concepts":[{"name":"X","sections":["type:A"]}]}"#;
        let e = Enrichment::parse(json, "a.md", &n).unwrap();
        assert!(e.concepts.is_empty());
        assert_eq!(e.rejected.len(), 1);
    }
}
