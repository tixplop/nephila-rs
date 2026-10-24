//! `query ask` (REQ-016) : question en langage naturel posée au LLM, avec pour seul contexte
//! des données du graphe (nœuds trouvés par les mots de la question et leurs voisins directs).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::out::Out;
use std::sync::atomic::AtomicBool;

use serde_json::{json, Value};

use super::lookup::{self, Lookup};
use super::search;
use crate::config::Config;
use crate::error::{Error, Result, TRUNCATED};
use crate::filter::ProjectFile;
use crate::llm::{build_llm, with_format_retries, AskReply, Llm};
use crate::model::{Edge, Node};
use crate::store::Store;

/// Nombre maximal de nœuds trouvés par les mots de la question.
const MAX_SEEDS: usize = 20;
/// Nombre maximal de nœuds (trouvés + voisins) décrits dans le contexte.
const MAX_NODES: usize = 60;
/// Taille maximale du contexte, en caractères.
const MAX_CONTEXT_CHARS: usize = 12_000;
/// Longueur minimale d'un mot de la question pour être cherché.
const MIN_WORD_CHARS: usize = 3;

const STOP_WORDS: &[&str] = &[
    "quel", "quelle", "quels", "quelles", "que", "qui", "quoi", "les", "des", "une", "est", "sont",
    "par", "pour", "dans", "sur", "avec", "comment", "pourquoi", "the", "what", "which", "how",
    "who", "where", "and", "does", "are", "for", "with",
];

pub fn not_found() -> Error {
    Error::Message("pas trouvé dans le graphe".to_string())
}

/// Mots significatifs de la question, minuscules, sans doublon, dans l'ordre d'apparition.
fn keywords(question: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    question
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= MIN_WORD_CHARS && !STOP_WORDS.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// Nœuds trouvés par les mots de la question, sans doublon, dans l'ordre de pertinence.
/// La recherche travaille sur des références ; seuls les nœuds retenus sont copiés.
fn find_seeds(nodes: &[Node], words: &[String]) -> Vec<Node> {
    let prepared = search::Prepared::new(nodes);
    let mut seen = HashSet::new();
    let mut seeds: Vec<Node> = Vec::new();
    for w in words {
        for n in prepared.matches(w) {
            if seeds.len() >= MAX_SEEDS {
                return seeds;
            }
            if seen.insert(n.id.as_str()) {
                seeds.push(n.clone());
            }
        }
    }
    seeds
}

fn node_line(n: &Node) -> String {
    let mut l = format!("- {} : {}", n.id, super::describe(n));
    if let Some(s) = &n.summary {
        l.push_str(" - ");
        l.push_str(s);
    }
    l
}

/// Contexte textuel : nœuds trouvés, voisins à profondeur 1 et liens entre eux. Ne contient que
/// des données du graphe, jamais de contenu de fichier source.
fn build_context<'a>(nodes: &'a [Node], edges: &'a [Edge], seeds: &'a [Node]) -> String {
    let by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut included: Vec<&Node> = Vec::new();
    let mut ids: HashSet<&str> = HashSet::new();
    let mut add = |n: &'a Node, included: &mut Vec<&'a Node>| {
        if included.len() < MAX_NODES && ids.insert(n.id.as_str()) {
            included.push(n);
        }
    };
    for s in seeds {
        add(s, &mut included);
    }
    let seed_ids: HashSet<&str> = seeds.iter().map(|n| n.id.as_str()).collect();
    for e in edges {
        for (a, b) in [(&e.source, &e.target), (&e.target, &e.source)] {
            if seed_ids.contains(a.as_str()) {
                if let Some(n) = by_id.get(b.as_str()) {
                    add(n, &mut included);
                }
            }
        }
    }
    let mut out = String::from("Nœuds :\n");
    for n in &included {
        out.push_str(&node_line(n));
        out.push('\n');
    }
    out.push_str("Liens :\n");
    for e in edges {
        if ids.contains(e.source.as_str())
            && ids.contains(e.target.as_str())
            && (seed_ids.contains(e.source.as_str()) || seed_ids.contains(e.target.as_str()))
        {
            out.push_str(&format!(
                "- {} -{}-> {}\n",
                e.source,
                e.kind.as_str(),
                e.target
            ));
        }
    }
    truncate_chars(&mut out, MAX_CONTEXT_CHARS);
    out
}

fn truncate_chars(s: &mut String, max: usize) {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
}

fn to_json(n: &Node) -> Value {
    json!({
        "kind": n.kind.as_str(),
        "subkind": n.subkind.map(|s| s.as_str()),
        "qualified": n.qualified,
        "file": n.file,
        "summary": n.summary,
    })
}

/// Pose la question ; retourne la réponse et les nœuds cités existant dans le graphe.
pub fn answer(
    store: &Store,
    llm: &dyn Llm,
    question: &str,
    format_retries: u32,
) -> Result<(String, Vec<Node>)> {
    let nodes = store.nodes()?;
    let seeds = find_seeds(&nodes, &keywords(question));
    if seeds.is_empty() {
        return Err(not_found());
    }
    let context = build_context(&nodes, &store.edges()?, &seeds);
    let stop = AtomicBool::new(false);
    let reply = with_format_retries(format_retries, &stop, |_| llm.ask(question, &context))
        .map_err(|e| match e {
            Error::Truncated(_) => Error::Truncated(format!(
                "{TRUNCATED} ; posez une question plus ciblée ou augmentez max_output_tokens"
            )),
            other => other,
        })?;
    match reply {
        AskReply::NotFound => Err(not_found()),
        AskReply::Found {
            answer,
            nodes: cited,
        } => {
            let mut seen = HashSet::new();
            let mut existing = Vec::new();
            for name in &cited {
                if let Lookup::Found(n) = lookup::resolve(store, name)? {
                    if seen.insert(n.id.clone()) {
                        existing.push(n);
                    }
                }
            }
            if existing.is_empty() {
                return Err(not_found());
            }
            Ok((answer, existing))
        }
    }
}

pub fn run(
    repo: &Path,
    question: &str,
    as_json: bool,
    check_stale: bool,
    format_retries: Option<u32>,
    out: &mut Out,
) -> Result<()> {
    if question.trim().is_empty() {
        return Err(Error::Message("la question est vide".to_string()));
    }
    let project = ProjectFile::load(repo)?;
    let config = Config::load()?;
    let (config, _) = config.with_project(&project.settings, None, format_retries);
    let llm = build_llm(&config)?;
    let store = super::open(repo, check_stale)?;
    let (answer, cited) = answer(&store, llm.as_ref(), question, config.format_retries)?;
    if as_json {
        let nodes: Vec<Value> = cited.iter().map(to_json).collect();
        writeln!(out, "{}", json!({"answer": answer, "nodes": nodes}))?;
    } else {
        writeln!(out, "{answer}")?;
        writeln!(out, "Nœuds cités :")?;
        for n in &cited {
            writeln!(out, "- {}", super::describe(n))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::search::oracle::*;
    use super::*;

    /// `find_seeds` de la 0.18.0 (copie du graphe par mot-clé), oracle.
    fn legacy_find_seeds(nodes: &[Node], words: &[String]) -> Vec<Node> {
        let mut seen = HashSet::new();
        let mut seeds = Vec::new();
        for w in words {
            for n in legacy_matches(nodes.to_vec(), w) {
                if seen.insert(n.id.clone()) {
                    seeds.push(n);
                }
            }
        }
        seeds.truncate(MAX_SEEDS);
        seeds
    }

    #[test]
    fn req055_ac1_find_seeds_same_ids_in_same_order_as_0_18_0() {
        let mut rng = Lcg(1055);
        let mut truncated = false;
        for _ in 0..200 {
            let nodes = random_nodes(&mut rng);
            let words = random_words(&mut rng);
            let old = legacy_find_seeds(&nodes, &words);
            truncated |= old.len() == MAX_SEEDS;
            assert_eq!(old, find_seeds(&nodes, &words), "mots {words:?}");
        }
        assert!(truncated, "le plafond de 20 nœuds n'est jamais atteint");
    }
}
