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

/// Filtre les éléments bruts d'un fichier selon sa liste de nœuds (règles version 2, REQ-024).
fn filter_raw(
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
    file: &str,
    node_ids: &[String],
) -> Enrichment {
    let known: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let mut out = Enrichment::default();

    for s in summaries {
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
    for c in concepts {
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
    out
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
        Ok(filter_raw(raw.summaries, raw.concepts, file, node_ids))
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

/// Version du schéma de réponse d'un lot de 2 fichiers ou plus (DEC-033).
pub const ENRICH_LOT_SCHEMA_VERSION: u32 = 3;

/// Un fichier d'un lot : chemin, contenu et identifiants de ses nœuds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotItem {
    pub file: String,
    pub text: String,
    pub node_ids: Vec<String>,
}

/// Résultat d'un lot : un résultat par fichier du lot (dans l'ordre du lot) et les éléments
/// écartés qui ne se rattachent à aucun fichier (entrée hors lot ou en double).
#[derive(Debug)]
pub struct LotEnrichment {
    pub files: Vec<(String, Result<Enrichment>)>,
    pub rejected: Vec<String>,
}

impl LotEnrichment {
    /// Résultats par fichier ; les éléments écartés propres au lot sont rattachés au premier
    /// fichier enrichi avec succès pour qu'ils soient signalés ; si aucun fichier n'a réussi,
    /// ils sont reportés dans le message d'erreur de chaque fichier absent.
    pub fn into_results(self) -> Vec<(String, Result<Enrichment>)> {
        let LotEnrichment {
            mut files,
            rejected,
        } = self;
        if let Some(Ok(e)) = files.iter_mut().map(|(_, r)| r).find(|r| r.is_ok()) {
            e.rejected.extend(rejected);
        } else if !rejected.is_empty() {
            let note = format!(" (éléments écartés du lot : {})", rejected.join(" ; "));
            for (_, r) in files.iter_mut() {
                if let Err(err) = r {
                    *r = Err(Error::Message(format!("{err}{note}")));
                }
            }
        }
        files
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLot {
    version: u32,
    files: Vec<RawLotEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLotEntry {
    file: String,
    summaries: Vec<RawSummary>,
    concepts: Vec<RawConcept>,
}

/// Analyse strictement la réponse version 3 d'un lot puis filtre chaque entrée selon les nœuds
/// du fichier qu'elle nomme. Une réponse illisible ou d'une autre version rend tout le lot
/// non conforme (`Err`) ; un fichier du lot sans entrée échoue seul.
pub fn parse_lot(json: &str, items: &[LotItem]) -> Result<LotEnrichment> {
    let raw: RawLot = serde_json::from_str(json).map_err(invalid)?;
    if raw.version != ENRICH_LOT_SCHEMA_VERSION {
        return Err(invalid(format!(
            "version {} non prise en charge pour un lot (attendu : {ENRICH_LOT_SCHEMA_VERSION})",
            raw.version
        )));
    }
    let mut rejected = Vec::new();
    let mut found: Vec<Option<Enrichment>> = vec![None; items.len()];
    for entry in raw.files {
        let Some(i) = items.iter().position(|it| it.file == entry.file) else {
            rejected.push(format!("entrée pour {} (fichier hors du lot)", entry.file));
            continue;
        };
        if found[i].is_some() {
            rejected.push(format!("entrée en double pour {}", entry.file));
            continue;
        }
        found[i] = Some(filter_raw(
            entry.summaries,
            entry.concepts,
            &entry.file,
            &items[i].node_ids,
        ));
    }
    let files = items
        .iter()
        .zip(found)
        .map(|(it, e)| {
            let r = e.ok_or_else(|| {
                Error::Message(format!("{} : absent de la réponse du lot", it.file))
            });
            (it.file.clone(), r)
        })
        .collect();
    Ok(LotEnrichment { files, rejected })
}

/// Prompt système d'un lot de fichiers (identique pour tous les fournisseurs).
pub fn lot_system_prompt() -> String {
    format!(
        "Tu enrichis le graphe d'un dépôt à partir de plusieurs fichiers (code Java/Kotlin/Groovy/Scala, \
         build Maven/Gradle/Ivy, ou Markdown) et tu réponds uniquement par un objet JSON, sans texte autour.\n\
         Chaque fichier est introduit par une ligne `=== FICHIER : <chemin> ===`, suivie de la liste de ses \
         nœuds puis de son contenu, jusqu'à la ligne `=== FIN DU FICHIER : <chemin> ===`.\n\
         Schéma (version {ENRICH_LOT_SCHEMA_VERSION}) : {{\"version\": {ENRICH_LOT_SCHEMA_VERSION}, \"files\": \
         [{{\"file\": \"<chemin exact du fichier>\", \"summaries\": [{{\"node\": \"<identifiant de la liste de ce fichier>\", \
         \"summary\": \"une phrase\"}}], \"concepts\": [{{\"name\": \"nom du concept\", \"summary\": \"une phrase (optionnel)\", \
         \"sections\": [\"<identifiant de section de la liste de ce fichier>\"]}}]}}]}}.\n\
         Produis exactement une entrée par fichier fourni. Les résumés ne portent que sur les nœuds de la \
         liste du fichier concerné. Les concepts ne concernent que les fichiers Markdown et ne citent que des \
         sections de la liste du même fichier. Tu ne crées, ne renomme ni ne supprimes aucun nœud et tu ne \
         produis aucun autre lien. Sans rien à signaler pour un fichier, donne-lui une entrée avec \
         \"summaries\": [] et \"concepts\": []."
    )
}

/// Message utilisateur d'un lot : pour chaque fichier, chemin, nœuds et contenu, avec séparateurs.
pub fn lot_user_prompt(items: &[LotItem]) -> String {
    let mut out = String::new();
    for it in items {
        let list: String = it.node_ids.iter().map(|n| format!("- {n}\n")).collect();
        out.push_str(&format!(
            "=== FICHIER : {f} ===\nNœuds du fichier :\n{list}\nContenu :\n{t}\n=== FIN DU FICHIER : {f} ===\n\n",
            f = it.file,
            t = it.text
        ));
    }
    out
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

    fn item(file: &str, nodes: &[&str]) -> LotItem {
        LotItem {
            file: file.into(),
            text: String::new(),
            node_ids: ids(nodes),
        }
    }

    #[test]
    fn req027_ac6_parse_lot_conforming() {
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"B.java","summaries":[{"node":"type:B","summary":"b"},{"node":"type:A","summary":"x"}],"concepts":[]},
            {"file":"A.java","summaries":[{"node":"type:A","summary":"a"}],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert!(l.rejected.is_empty());
        assert_eq!(l.files[0].0, "A.java");
        let a = l.files[0].1.as_ref().unwrap();
        assert_eq!(a.summaries, vec![("type:A".into(), "a".into())]);
        let b = l.files[1].1.as_ref().unwrap();
        assert_eq!(b.summaries, vec![("type:B".into(), "b".into())]);
        assert_eq!(b.rejected.len(), 1, "{:?}", b.rejected);
    }

    #[test]
    fn req027_ac8_parse_lot_non_conforming() {
        let items = [item("A.java", &[]), item("B.java", &[])];
        for bad in [
            "pas du json",
            r#"{"version":2,"files":[]}"#,
            r#"{"version":3,"files":[],"extra":1}"#,
            r#"{"version":3,"files":[{"file":"A.java","summaries":[],"concepts":[],"x":1}]}"#,
            r#"{"version":3,"files":[{"file":"A.java","summaries":[]}]}"#,
        ] {
            assert!(parse_lot(bad, &items).is_err(), "{bad}");
        }
    }

    #[test]
    fn req027_ac7_parse_lot_out_of_lot_duplicate_and_missing() {
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"A.java","summaries":[{"node":"type:A","summary":"a"}],"concepts":[]},
            {"file":"autre/Inconnu.java","summaries":[],"concepts":[]},
            {"file":"A.java","summaries":[{"node":"type:A","summary":"bis"}],"concepts":[]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        assert_eq!(l.rejected.len(), 2, "{:?}", l.rejected);
        assert!(l.rejected[0].contains("autre/Inconnu.java"));
        assert!(l.rejected[1].contains("A.java"));
        let a = l.files[0].1.as_ref().unwrap();
        assert_eq!(a.summaries, vec![("type:A".into(), "a".into())]);
        let e = l.files[1].1.as_ref().unwrap_err().to_string();
        assert!(
            e.contains("B.java") && e.contains("absent de la réponse du lot"),
            "{e}"
        );
        let r = l.into_results();
        let a = r[0].1.as_ref().unwrap();
        assert_eq!(a.rejected.len(), 2);
    }

    #[test]
    fn req027_ac4_parse_lot_concept_in_java_entry_is_dropped() {
        let items = [
            item("A.java", &["type:A"]),
            item("d.md", &["section:d.md#s"]),
        ];
        let json = r#"{"version":3,"files":[
            {"file":"A.java","summaries":[],"concepts":[{"name":"X","sections":["type:A"]}]},
            {"file":"d.md","summaries":[],"concepts":[{"name":"Y","sections":["section:d.md#s"]}]}]}"#;
        let l = parse_lot(json, &items).unwrap();
        let a = l.files[0].1.as_ref().unwrap();
        assert!(a.concepts.is_empty());
        assert_eq!(a.rejected.len(), 1);
        assert_eq!(l.files[1].1.as_ref().unwrap().concepts.len(), 1);
    }

    #[test]
    fn req027_ac7_parse_lot_all_out_of_lot_reports_rejected_in_errors() {
        let items = [item("A.java", &["type:A"]), item("B.java", &["type:B"])];
        let json = r#"{"version":3,"files":[
            {"file":"X.java","summaries":[],"concepts":[]},
            {"file":"Y.java","summaries":[],"concepts":[]}]}"#;
        let r = parse_lot(json, &items).unwrap().into_results();
        assert_eq!(r.len(), 2);
        for (f, res) in &r {
            let e = res.as_ref().unwrap_err().to_string();
            assert!(
                e.contains(f) && e.contains("absent de la réponse du lot"),
                "{e}"
            );
            assert!(e.contains("X.java") && e.contains("Y.java"), "{e}");
        }
    }
}
