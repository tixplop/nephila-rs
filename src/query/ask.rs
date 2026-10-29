//! `query ask` (REQ-016) : question en langage naturel posée au LLM, avec pour seul contexte
//! des données du graphe (nœuds trouvés par les mots de la question et leurs voisins directs).
//! Les nœuds cités par la réponse sont résolus parmi les seuls nœuds montrés dans ce contexte ;
//! une réponse sans citation retenue est affichée avec un avertissement (REQ-161, DEC-130).

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
use crate::llm::{build_llm, calls_line, with_format_retries, AskReply, CallStats, Llm, LlmTotals};
use crate::model::{Edge, Node};
use crate::store::Store;

pub use crate::config::{
    AskLimits, ASK_MIN_CONTEXT_CHARS, DEFAULT_ASK_MAX_CONTEXT_CHARS, DEFAULT_ASK_MAX_NODES,
    DEFAULT_ASK_MAX_SEEDS,
};

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

/// Noms qualifiés cités par la question (REQ-141) : suites d'au moins deux segments séparés
/// chacun par un seul `.` ou `#`, en minuscules, sans doublon, dans l'ordre d'apparition.
fn qualified_terms(question: &str) -> Vec<String> {
    let is_name = |c: char| c.is_alphanumeric() || c == '_';
    let mut out: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut chain = String::new();
    let mut segments = 0;
    let mut flush = |chain: &mut String, segments: &mut usize| {
        if *segments >= 2 && seen.insert(chain.clone()) {
            out.push(chain.clone());
        }
        chain.clear();
        *segments = 0;
    };
    let chars: Vec<char> = question.to_lowercase().chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let name = is_name(chars[i]);
        while i < chars.len() && is_name(chars[i]) == name {
            i += 1;
        }
        let run = &chars[start..i];
        if name {
            chain.extend(run);
            segments += 1;
        } else if run.len() == 1 && matches!(run[0], '.' | '#') && segments > 0 && i < chars.len() {
            chain.push(run[0]);
        } else {
            if chain.ends_with(['.', '#']) {
                chain.pop();
            }
            flush(&mut chain, &mut segments);
        }
    }
    if chain.ends_with(['.', '#']) {
        chain.pop();
    }
    flush(&mut chain, &mut segments);
    out
}

/// Identifiants cités par la question (REQ-141) : mots-clés dont une occurrence est un segment
/// d'un mot écrit en forme de code. Un mot écrit est une suite de segments séparés chacun par
/// un seul `.` ou `#` ; il est en forme de code s'il compte au moins deux segments, contient
/// `_`, a un segment avec une majuscule hors première position et une minuscule, ou est suivi
/// immédiatement de `()`.
fn cited_identifiers(question: &str) -> HashSet<String> {
    let is_name = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<char> = question.chars().collect();
    let mut out = HashSet::new();
    let mut add_word = |segments: &[String], called: bool| {
        let mixed_case = |s: &String| {
            s.chars().skip(1).any(char::is_uppercase) && s.chars().any(char::is_lowercase)
        };
        let code = called
            || segments.len() >= 2
            || segments.iter().any(|s| s.contains('_') || mixed_case(s));
        if code {
            for s in segments {
                let w = s.to_lowercase();
                if w.chars().count() >= MIN_WORD_CHARS && !STOP_WORDS.contains(&w.as_str()) {
                    out.insert(w);
                }
            }
        }
    };
    let mut segments: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_name(chars[i]) {
            let start = i;
            while i < chars.len() && is_name(chars[i]) {
                i += 1;
            }
            segments.push(chars[start..i].iter().collect());
            let joined =
                i + 1 < chars.len() && matches!(chars[i], '.' | '#') && is_name(chars[i + 1]);
            if joined {
                i += 1;
            } else {
                let called = chars[i..].starts_with(&['(', ')']);
                add_word(&segments, called);
                segments.clear();
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Clé de classement d'un nœud pour un terme : (classe, fréquence, rang, position du terme).
type SeedKey = (u8, usize, u8, usize);

/// Classe d'un mot-clé : 1 ou 2 (exact / autre) pour un identifiant cité, 3 ou 4 sinon.
fn word_class(cited: bool, rank: u8) -> u8 {
    (if cited { 1 } else { 3 }) + u8::from(rank != 0)
}

/// Nœuds de départ : les nœuds qui correspondent à au moins un terme (noms qualifiés cités puis
/// mots-clés, ceux qui sont des identifiants cités avant les autres), triés par leur meilleure clé, puis nom qualifié et identifiant ; les `min(max_seeds, max_nodes)` premiers.
/// La recherche travaille sur des références ; seuls les nœuds retenus sont copiés.
fn find_seeds(
    nodes: &[Node],
    qualified: &[String],
    words: &[String],
    cited: &HashSet<String>,
    limits: &AskLimits,
) -> Vec<Node> {
    let prepared = search::Prepared::new(nodes);
    let mut best: Vec<Option<SeedKey>> = vec![None; nodes.len()];
    let mut offer = |i: usize, key: SeedKey| {
        if best[i].is_none_or(|b| key < b) {
            best[i] = Some(key);
        }
    };
    for (pos, name) in qualified.iter().enumerate() {
        let hits = prepared.qualified_suffix(name);
        let freq = hits.len();
        for i in hits {
            offer(i, (0, freq, 0, pos));
        }
    }
    for (k, w) in words.iter().enumerate() {
        let hits = prepared.ranks(w.trim().to_lowercase().as_str());
        let freq = hits.len();
        for (i, r) in hits {
            offer(
                i,
                (
                    word_class(cited.contains(w), r),
                    freq,
                    r,
                    qualified.len() + k,
                ),
            );
        }
    }
    let mut hit: Vec<(SeedKey, usize)> = best
        .iter()
        .enumerate()
        .filter_map(|(i, k)| k.map(|k| (k, i)))
        .collect();
    hit.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| nodes[a.1].qualified.cmp(&nodes[b.1].qualified))
            .then_with(|| nodes[a.1].id.cmp(&nodes[b.1].id))
    });
    let mut seen = HashSet::new();
    hit.into_iter()
        .filter(|&(_, i)| seen.insert(nodes[i].id.as_str()))
        .take(limits.max_seeds.min(limits.max_nodes))
        .map(|(_, i)| nodes[i].clone())
        .collect()
}

fn node_line(n: &Node) -> String {
    let mut l = format!("- {} : {}", n.id, super::describe(n));
    if let Some(s) = &n.summary {
        l.push_str(" - ");
        l.push_str(s);
    }
    l
}

/// Remplissage du contexte : lignes de nœuds et de liens écrites, et place restante (REQ-142).
struct Fill<'a> {
    max_chars: usize,
    nodes_text: String,
    links_text: String,
    written: HashSet<&'a str>,
    /// Nœuds écrits, dans l'ordre des lignes (REQ-161).
    shown: Vec<&'a Node>,
    neighbours: usize,
    used: usize,
}

impl<'a> Fill<'a> {
    /// Écrit une étape (ligne de nœud éventuelle et ligne de lien éventuelle) si elle tient.
    fn push(&mut self, node: Option<&'a Node>, link: Option<&Edge>) -> bool {
        let node_text = node.map(|n| format!("{}\n", node_line(n)));
        let link_text =
            link.map(|e| format!("- {} -{}-> {}\n", e.source, e.kind.as_str(), e.target));
        let cost: usize = [&node_text, &link_text]
            .into_iter()
            .flatten()
            .map(|t| t.chars().count())
            .sum();
        if self.used + cost > self.max_chars {
            return false;
        }
        self.used += cost;
        if let (Some(n), Some(t)) = (node, node_text) {
            self.written.insert(n.id.as_str());
            self.shown.push(n);
            self.nodes_text.push_str(&t);
        }
        if let Some(t) = link_text {
            self.links_text.push_str(&t);
        }
        true
    }
}

/// Contexte envoyé au LLM : son texte et les nœuds montrés, dans l'ordre des lignes de nœuds
/// (REQ-161, DEC-130).
struct Context<'a> {
    text: String,
    shown: Vec<&'a Node>,
}

/// Contexte textuel : nœuds de départ dans l'ordre de priorité, chacun suivi de ses liens et de
/// leurs voisins, tant que le contexte tient (REQ-142). Ne contient que des données du graphe,
/// jamais de contenu de fichier source. Rend aussi les nœuds montrés (lignes de nœuds écrites,
/// dans l'ordre ; extrémités des liens comprises) : les citations de la réponse sont résolues
/// parmi eux (REQ-161, DEC-130). Le premier nœud de départ coupé n'est montré que si sa ligne
/// coupée contient encore `- <identifiant> :`.
fn build_context<'a>(
    nodes: &'a [Node],
    edges: &'a [Edge],
    seeds: &'a [Node],
    limits: &AskLimits,
) -> Context<'a> {
    const NODES_HEADER: &str = "Nœuds :\n";
    const LINKS_HEADER: &str = "Liens :\n";
    let header_chars = NODES_HEADER.chars().count() + LINKS_HEADER.chars().count();
    let by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let seed_ids: HashSet<&str> = seeds.iter().map(|n| n.id.as_str()).collect();
    // Liens qui touchent chaque nœud de départ, dans l'ordre de la base (un seul passage).
    let mut adjacent: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for e in edges {
        if seed_ids.contains(e.source.as_str()) {
            adjacent.entry(e.source.as_str()).or_default().push(e);
        }
        if e.target != e.source && seed_ids.contains(e.target.as_str()) {
            adjacent.entry(e.target.as_str()).or_default().push(e);
        }
    }
    let neighbour_cap = limits.max_nodes.saturating_sub(seed_ids.len());
    let mut fill = Fill {
        max_chars: limits.max_context_chars,
        nodes_text: String::new(),
        links_text: String::new(),
        written: HashSet::new(),
        shown: Vec::new(),
        neighbours: 0,
        used: header_chars,
    };
    let mut done: HashSet<(&str, &str, &str)> = HashSet::new();
    'seeds: for (rank, s) in seeds.iter().enumerate() {
        if !fill.written.contains(s.id.as_str()) && !fill.push(Some(s), None) {
            if rank == 0 {
                // Seule coupe dans une ligne : celle du premier nœud de départ, seul.
                let room = limits.max_context_chars.saturating_sub(header_chars + 1);
                let cut: String = node_line(s).chars().take(room).collect();
                if cut.starts_with(&format!("- {} :", s.id)) {
                    fill.shown.push(s);
                }
                fill.nodes_text = format!("{cut}\n");
            }
            break;
        }
        for e in adjacent.get(s.id.as_str()).into_iter().flatten() {
            let key = (e.source.as_str(), e.target.as_str(), e.kind.as_str());
            if done.contains(&key) {
                continue;
            }
            let other = if e.source == s.id {
                &e.target
            } else {
                &e.source
            };
            let Some(&other_node) = by_id.get(other.as_str()) else {
                continue;
            };
            let is_seed = seed_ids.contains(other.as_str());
            let new_node = if fill.written.contains(other.as_str()) {
                None
            } else if is_seed || fill.neighbours < neighbour_cap {
                Some(other_node)
            } else {
                continue;
            };
            if !fill.push(new_node, Some(e)) {
                break 'seeds;
            }
            done.insert(key);
            if new_node.is_some() && !is_seed {
                fill.neighbours += 1;
            }
        }
    }
    Context {
        text: format!(
            "{NODES_HEADER}{}{LINKS_HEADER}{}",
            fill.nodes_text, fill.links_text
        ),
        shown: fill.shown,
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

/// Pose la question ; retourne la réponse et les nœuds cités retenus, éventuellement aucun
/// (REQ-161, DEC-130). Chaque citation est résolue parmi les seuls nœuds montrés dans le
/// contexte envoyé, sans requête à la base ; une citation introuvable ou ambiguë parmi eux est
/// écartée. `found: false` et l'absence de nœud de départ donnent « pas trouvé ».
pub fn answer(
    store: &Store,
    llm: &dyn Llm,
    question: &str,
    format_retries: u32,
    limits: &AskLimits,
) -> Result<(String, Vec<Node>)> {
    let nodes = store.nodes()?;
    let seeds = find_seeds(
        &nodes,
        &qualified_terms(question),
        &keywords(question),
        &cited_identifiers(question),
        limits,
    );
    if seeds.is_empty() {
        return Err(not_found());
    }
    let edges = store.edges()?;
    let context = build_context(&nodes, &edges, &seeds, limits);
    let stop = AtomicBool::new(false);
    let mut totals = LlmTotals::default();
    let result = with_format_retries(format_retries, &stop, |attempt| {
        let stats = CallStats::default();
        let r = llm.ask_with(question, &context.text, &stats);
        totals.add(&stats, attempt >= 2);
        r
    });
    if totals.calls > 0 {
        crate::trace!("{}", calls_line(&totals));
    }
    let reply = result.map_err(|e| match e {
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
            let retained = retain_cited(&context.shown, &cited);
            Ok((answer, retained))
        }
    }
}

/// Citations résolues parmi les nœuds montrés (REQ-161), dans l'ordre des citations, sans doublon.
fn retain_cited(shown: &[&Node], cited: &[String]) -> Vec<Node> {
    let mut seen = HashSet::new();
    let mut retained = Vec::new();
    for name in cited {
        if let Lookup::Found(n) = lookup::resolve_among(shown.iter().copied(), name) {
            if seen.insert(n.id.clone()) {
                retained.push(n);
            }
        }
    }
    retained
}

/// Avertissement A-9 : réponse affichée sans aucun nœud cité retenu (V-006).
const UNVERIFIABLE: &str = "réponse non vérifiable : aucun nœud cité ne figure dans le contexte envoyé ; vérifiez-la avec nephila query search";

/// Options propres à `query ask` (REQ-038, REQ-162, REQ-163).
#[derive(Clone, Copy, Debug, Default)]
pub struct AskOptions {
    pub verbose: bool,
    pub format_retries: Option<u32>,
    pub context_chars: Option<usize>,
}

pub fn run(
    repo: &Path,
    question: &str,
    as_json: bool,
    check_stale: bool,
    opts: AskOptions,
    out: &mut Out,
) -> Result<()> {
    let AskOptions {
        verbose,
        format_retries,
        context_chars,
    } = opts;
    crate::trace::init(verbose);
    if question.trim().is_empty() {
        return Err(Error::Message("la question est vide".to_string()));
    }
    let project = ProjectFile::load(repo)?;
    let config = Config::load()?;
    let (config, limits, marks) = config.for_ask(&project.settings, format_retries, context_chars);
    let llm = build_llm(&config)?;
    let store = super::open(repo, check_stale)?;
    let mark = if marks.is_empty() {
        String::new()
    } else {
        format!(" (.nephila.toml : {})", marks.join(","))
    };
    crate::trace!(
        "config: llm={} model={} thinking={} effort={} max_output_tokens={} max_context_chars={} max_seeds={} max_nodes={} timeout_secs={} format_retries={}{}{}",
        config.provider.as_str(),
        config.model,
        config.thinking,
        config.effort.as_str(),
        config.max_output_tokens,
        limits.max_context_chars,
        limits.max_seeds,
        limits.max_nodes,
        config.timeout_secs,
        config.format_retries,
        config.tls_trace(),
        mark
    );
    let (answer, cited) = answer(
        &store,
        llm.as_ref(),
        question,
        config.format_retries,
        &limits,
    )?;
    if cited.is_empty() {
        crate::error::warn(UNVERIFIABLE);
    }
    if as_json {
        let nodes: Vec<Value> = cited.iter().map(to_json).collect();
        writeln!(out, "{}", json!({"answer": answer, "nodes": nodes}))?;
    } else {
        writeln!(out, "{}", crate::escape::ask_answer(&answer))?;
        if !cited.is_empty() {
            writeln!(out, "Nœuds cités :")?;
            for n in &cited {
                let l = format!("- {}", super::describe(n));
                writeln!(out, "{}", crate::escape::data(&l))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::search::oracle::*;
    use super::*;
    use crate::model::NodeKind;

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
        seeds.truncate(DEFAULT_ASK_MAX_SEEDS);
        seeds
    }

    fn mk(kind: NodeKind, q: &str, summary: Option<&str>) -> Node {
        let short = q.rsplit(['.', '#']).next().unwrap();
        Node::new(kind, q, short, Some("A.java"), None, summary)
    }

    fn javapoet_graph() -> Vec<Node> {
        let p = "com.squareup.javapoet";
        let mut v = vec![
            mk(NodeKind::Package, p, None),
            mk(NodeKind::Type, &format!("{p}.MethodSpec"), None),
        ];
        for i in 0..105 {
            v.push(mk(NodeKind::Method, &format!("{p}.MethodSpec.m{i}"), None));
        }
        v.push(mk(NodeKind::Type, &format!("{p}.CodeWriter"), None));
        for i in 0..31 {
            v.push(mk(
                NodeKind::Method,
                &format!("{p}.CodeWriter.emit{i}"),
                None,
            ));
        }
        v.push(mk(
            NodeKind::Method,
            &format!("{p}.CodeWriter.suggestedImports"),
            None,
        ));
        v.push(mk(NodeKind::Type, &format!("{p}.JavaFile"), None));
        v.push(mk(NodeKind::Method, &format!("{p}.JavaFile.writeTo"), None));
        v
    }

    fn seed_ids(nodes: &[Node], question: &str) -> Vec<String> {
        find_seeds(
            nodes,
            &qualified_terms(question),
            &keywords(question),
            &cited_identifiers(question),
            &AskLimits::default(),
        )
        .into_iter()
        .map(|n| n.id)
        .collect()
    }

    #[test]
    fn req141_ac1_named_node_leads_the_seeds() {
        let nodes = javapoet_graph();
        for q in [
            "Which method calls CodeWriter.suggestedImports()?",
            "Which calls CodeWriter.suggestedImports()?",
            "Who calls suggestedImports in CodeWriter?",
        ] {
            let ids = seed_ids(&nodes, q);
            assert_eq!(ids.len(), 20, "{q}");
            assert_eq!(
                ids[0], "method:com.squareup.javapoet.CodeWriter.suggestedImports",
                "{q}"
            );
            assert_eq!(ids[1], "type:com.squareup.javapoet.CodeWriter", "{q}");
            let unique: HashSet<&String> = ids.iter().collect();
            assert_eq!(unique.len(), 20, "{q}");
        }
    }

    /// Graphe de la question Q08 de javapoet : `typespec` (164 nœuds), `build` (118) et
    /// `methods` (99) ; seul le type `TypeSpec` correspond à `typespec` sans les deux autres.
    fn q08_graph() -> Vec<Node> {
        let p = "com.squareup.javapoet";
        let mut v = vec![
            mk(NodeKind::Type, &format!("{p}.TypeSpec"), None),
            mk(NodeKind::Type, &format!("{p}.TypeSpecTest"), None),
        ];
        for i in 0..162 {
            v.push(mk(
                NodeKind::Method,
                &format!("{p}.TypeSpecTest.t{i}"),
                None,
            ));
        }
        for i in 0..118 {
            v.push(mk(NodeKind::Method, &format!("{p}.B{i}.build"), None));
        }
        v.push(mk(NodeKind::Section, "README.md#methods", None));
        for i in 0..98 {
            v.push(mk(
                NodeKind::Method,
                &format!("{p}.Doc.d{i}"),
                Some("lists methods"),
            ));
        }
        v
    }

    const Q08: &str = "Which kinds of types can TypeSpec build, and with which factory methods?";

    #[test]
    fn req141_ac7_cited_identifier_leads_over_ordinary_words() {
        let nodes = q08_graph();
        assert_eq!(
            nodes
                .iter()
                .filter(|n| n.qualified.to_lowercase().contains("typespec"))
                .count(),
            164
        );
        let ids = seed_ids(&nodes, Q08);
        assert_eq!(ids.len(), 20);
        assert_eq!(ids[0], "type:com.squareup.javapoet.TypeSpec");
        assert!(
            ids.iter().all(|i| i.to_lowercase().contains("typespec")),
            "{ids:?}"
        );
    }

    #[test]
    fn req141_ac2_qualified_terms() {
        let qt = qualified_terms;
        assert_eq!(
            qt("CodeWriter.suggestedImports()"),
            vec!["codewriter.suggestedimports"]
        );
        assert_eq!(
            qt("JavaFile.Builder.skipJavaLangImports(true)"),
            vec!["javafile.builder.skipjavalangimports"]
        );
        assert_eq!(qt("README.md#usage"), vec!["readme.md#usage"]);
        assert_eq!(qt("e.g. this"), vec!["e.g"]);
        assert!(qt("Who calls suggestedImports in CodeWriter?").is_empty());
        for q in ["a..b", ".a", "a."] {
            assert!(qt(q).is_empty(), "{q}");
        }
        assert_eq!(qt("A.b A.B"), vec!["a.b"]);
        assert_eq!(qt("x.y#z"), vec!["x.y#z"]);
    }

    #[test]
    fn req141_ac3_qualified_name_matches_on_segment_boundary() {
        let q = |s: &str| mk(NodeKind::Method, s, None);
        let nodes = vec![
            q("com.squareup.javapoet.CodeWriter.suggestedImports"),
            q("CodeWriter.suggestedImports"),
            q("com.x.MyCodeWriter.suggestedImports"),
            q("com.x.CodeWriter.suggestedImportsAll"),
            mk(NodeKind::Document, "docs/README.md#usage", None),
        ];
        let ids = |term: &str| -> Vec<String> {
            find_seeds(
                &nodes,
                &[term.to_string()],
                &[],
                &HashSet::new(),
                &AskLimits::default(),
            )
            .into_iter()
            .map(|n| n.qualified)
            .collect()
        };
        let mut got = ids("codewriter.suggestedimports");
        got.sort();
        assert_eq!(
            got,
            vec![
                "CodeWriter.suggestedImports",
                "com.squareup.javapoet.CodeWriter.suggestedImports"
            ]
        );
        assert_eq!(ids("readme.md#usage"), vec!["docs/README.md#usage"]);
    }

    fn seeds_of(nodes: &[Node], words: &[&str]) -> Vec<String> {
        let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        find_seeds(nodes, &[], &w, &HashSet::new(), &AskLimits::default())
            .into_iter()
            .map(|n| n.qualified)
            .collect()
    }

    #[test]
    fn req141_ac4_ordering() {
        let t = |q: &str, s: Option<&str>| mk(NodeKind::Type, q, s);
        // (a) le terme rare d'abord
        let mut nodes: Vec<Node> = (0..5)
            .map(|i| t(&format!("p.a{i}"), Some("alpha")))
            .collect();
        nodes.extend((0..3).map(|i| t(&format!("p.b{i}"), Some("beta"))));
        let got = seeds_of(&nodes, &["alpha", "beta"]);
        assert_eq!(got[..3], ["p.b0", "p.b1", "p.b2"]);
        assert_eq!(got.len(), 8);
        // (b) exact d'un mot fréquent avant préfixe d'un mot rare
        let mut nodes: Vec<Node> = (0..49)
            .map(|i| t(&format!("p.n{i:02}"), Some("common")))
            .collect();
        nodes.push(t("p.common", None));
        nodes.push(t("p.zrare1", None));
        nodes.push(t("p.zrare2", None));
        let got = seeds_of(&nodes, &["zrar", "common"]);
        assert_eq!(got[0], "p.common");
        // (c) nom qualifié cité avant correspondance exacte
        let nodes = vec![t("p.foo", None), t("q.Bar.baz", None), t("r.baz", None)];
        let got: Vec<String> = find_seeds(
            &nodes,
            &["bar.baz".to_string()],
            &["foo".to_string()],
            &HashSet::new(),
            &AskLimits::default(),
        )
        .into_iter()
        .map(|n| n.qualified)
        .collect();
        assert_eq!(got, ["q.Bar.baz", "p.foo"]);
        // (d) un nœud à deux termes, une seule fois, à la place de sa meilleure clé
        let nodes = vec![
            t("p.xray", Some("yank")),
            t("p.yank1", None),
            t("p.yank2", None),
        ];
        let got = seeds_of(&nodes, &["yank", "xray"]);
        assert_eq!(got, ["p.xray", "p.yank1", "p.yank2"]);
        // (e) plus de 20 correspondances : les 20 premiers de l'ordre défini
        let nodes: Vec<Node> = (0..30).map(|i| t(&format!("p.item{i:02}"), None)).collect();
        let got = seeds_of(&nodes, &["item"]);
        let want: Vec<String> = (0..20).map(|i| format!("p.item{i:02}")).collect();
        assert_eq!(got, want);
    }

    fn set(words: &[&str]) -> HashSet<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn req141_ac8_cited_identifiers() {
        let cases: &[(&str, &[&str])] = &[
            (
                "Which kinds of types can TypeSpec build, and with which factory methods?",
                &["typespec"],
            ),
            (
                "Which method calls CodeWriter.suggestedImports()?",
                &["codewriter", "suggestedimports"],
            ),
            (
                "Who calls suggestedImports in CodeWriter?",
                &["suggestedimports", "codewriter"],
            ),
            (
                "What is the effect of JavaFile.Builder.skipJavaLangImports(true)?",
                &["javafile", "builder", "skipjavalangimports"],
            ),
            ("TypeSpec.build and build", &["typespec", "build"]),
            ("What does build() return?", &["build"]),
            ("What is max_tokens?", &["max_tokens"]),
            ("What does Builder do with HTML and JSON?", &[]),
            (
                "Which classes extend TypeName in JavaPoet?",
                &["typename", "javapoet"],
            ),
            ("Where is it defined. Then build.", &[]),
            ("e.g. build", &[]),
        ];
        for (q, want) in cases {
            assert_eq!(cited_identifiers(q), set(want), "{q}");
        }
    }

    #[test]
    fn req141_ac9_ordering_with_cited_identifiers() {
        let t = |q: &str, s: Option<&str>| mk(NodeKind::Type, q, s);
        let w = |ws: &[&str]| -> Vec<String> { ws.iter().map(|s| s.to_string()).collect() };
        let seeds =
            |nodes: &[Node], quals: &[String], ws: &[&str], cited: &[&str]| -> Vec<String> {
                find_seeds(nodes, quals, &w(ws), &set(cited), &AskLimits::default())
                    .into_iter()
                    .map(|n| n.qualified)
                    .collect()
            };
        // (a) identifiant cité fréquent (préfixe) avant mot ordinaire rare (exact)
        let mut nodes: Vec<Node> = (0..50)
            .map(|i| t(&format!("p.common{i:02}"), None))
            .collect();
        nodes.push(t("p.rare", None));
        let got = seeds(&nodes, &[], &["rare", "common"], &["common"]);
        assert_eq!(got.len(), 20);
        assert!(got.iter().all(|q| q.starts_with("p.common")), "{got:?}");
        assert_eq!(seeds(&nodes, &[], &["rare", "common"], &[])[0], "p.rare");
        // (b) exact d'un identifiant fréquent avant préfixe d'un identifiant rare
        let mut nodes: Vec<Node> = (0..49)
            .map(|i| t(&format!("p.n{i:02}"), Some("common")))
            .collect();
        nodes.push(t("p.common", None));
        nodes.push(t("p.zrare1", None));
        nodes.push(t("p.zrare2", None));
        let got = seeds(&nodes, &[], &["zrar", "common"], &["zrar", "common"]);
        assert_eq!(got[0], "p.common");
        // (c) nom qualifié cité avant exact d'un identifiant cité
        let nodes = vec![t("p.foo", None), t("q.Bar.baz", None), t("r.baz", None)];
        let got = seeds(&nodes, &["bar.baz".to_string()], &["foo"], &["foo"]);
        assert_eq!(got, ["q.Bar.baz", "p.foo"]);
    }

    /// Oracle naïf d'AC-141-5, écrit depuis la définition.
    fn naive_seeds(
        nodes: &[Node],
        quals: &[String],
        words: &[String],
        cited: &HashSet<String>,
    ) -> Vec<String> {
        let lower = |s: &str| s.to_lowercase();
        let suffix = |q: &str, t: &str| {
            q == t
                || q.strip_suffix(t)
                    .and_then(|h| h.chars().next_back())
                    .is_some_and(|c| matches!(c, '.' | '#' | '/' | ':'))
        };
        let hit = |n: &Node, w: &str| -> Option<u8> {
            let (s, q) = (lower(&n.short), lower(&n.qualified));
            let sum = n.summary.as_deref().map(lower);
            if s == w || q == w {
                Some(0)
            } else if s.starts_with(w) || q.starts_with(w) {
                Some(1)
            } else if s.contains(w) || q.contains(w) || sum.is_some_and(|x| x.contains(w)) {
                Some(2)
            } else {
                None
            }
        };
        let mut keyed: Vec<(SeedKey, &Node)> = Vec::new();
        for n in nodes {
            let mut best = None;
            for (pos, t) in quals.iter().chain(words.iter()).enumerate() {
                let is_q = pos < quals.len();
                let freq = nodes
                    .iter()
                    .filter(|m| {
                        if is_q {
                            suffix(&lower(&m.qualified), t)
                        } else {
                            hit(m, t).is_some()
                        }
                    })
                    .count();
                let key = if is_q {
                    suffix(&lower(&n.qualified), t).then_some((0, freq, 0, pos))
                } else {
                    hit(n, t).map(|r| {
                        let base = if cited.contains(t) { 1 } else { 3 };
                        (if r == 0 { base } else { base + 1 }, freq, r, pos)
                    })
                };
                if let Some(k) = key {
                    best = Some(best.map_or(k, |b: SeedKey| b.min(k)));
                }
            }
            if let Some(k) = best {
                keyed.push((k, n));
            }
        }
        keyed.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.qualified.cmp(&b.1.qualified))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        let mut seen = HashSet::new();
        keyed
            .into_iter()
            .filter(|(_, n)| seen.insert(n.id.clone()))
            .take(DEFAULT_ASK_MAX_SEEDS)
            .map(|(_, n)| n.id.clone())
            .collect()
    }

    #[test]
    fn req141_ac5_find_seeds_matches_naive_oracle() {
        let mut rng = Lcg(1141);
        let mut capped = false;
        for _ in 0..200 {
            let nodes = random_nodes(&mut rng);
            let mut words = random_words(&mut rng);
            let total = 1 + rng.next(10);
            let nq = rng.next(total.min(3) + 1);
            words.truncate(total - nq);
            let mut quals = Vec::new();
            for _ in 0..nq {
                let q = nodes[rng.next(nodes.len())].qualified.to_lowercase();
                let cut: Vec<usize> = q.match_indices(['.', '#']).map(|(i, _)| i + 1).collect();
                let start = if cut.is_empty() {
                    0
                } else {
                    cut[rng.next(cut.len())]
                };
                quals.push(q[start..].to_string());
            }
            let cited: HashSet<String> =
                words.iter().filter(|_| rng.next(2) == 0).cloned().collect();
            let want = naive_seeds(&nodes, &quals, &words, &cited);
            capped |= want.len() == DEFAULT_ASK_MAX_SEEDS;
            let got: Vec<String> =
                find_seeds(&nodes, &quals, &words, &cited, &AskLimits::default())
                    .into_iter()
                    .map(|n| n.id)
                    .collect();
            assert_eq!(want, got, "noms {quals:?} mots {words:?}");
        }
        assert!(capped, "le plafond de 20 nœuds n'est jamais atteint");
    }

    #[test]
    fn req055_ac1_find_seeds_same_ids_in_same_order_as_0_18_0() {
        let mut rng = Lcg(1055);
        let mut truncated = false;
        for _ in 0..200 {
            let nodes = random_nodes(&mut rng);
            let words = random_words(&mut rng)[..1].to_vec();
            let old = legacy_find_seeds(&nodes, &words);
            truncated |= old.len() == DEFAULT_ASK_MAX_SEEDS;
            let cited: HashSet<String> =
                words.iter().filter(|_| rng.next(2) == 0).cloned().collect();
            assert_eq!(
                old,
                find_seeds(&nodes, &[], &words, &cited, &AskLimits::default()),
                "mots {words:?}"
            );
        }
        assert!(truncated, "le plafond de 20 nœuds n'est jamais atteint");
    }

    fn edge(source: &Node, target: &Node, kind: crate::model::EdgeKind) -> Edge {
        Edge {
            source: source.id.clone(),
            target: target.id.clone(),
            kind,
        }
    }

    fn link_line(e: &Edge) -> String {
        format!("- {} -{}-> {}", e.source, e.kind.as_str(), e.target)
    }

    /// Lignes de nœuds et lignes de liens d'un contexte.
    fn sections(ctx: &str) -> (Vec<&str>, Vec<&str>) {
        let body = ctx.strip_prefix("Nœuds :\n").expect("en-tête des nœuds");
        let (n, l) = body.split_once("Liens :\n").expect("en-tête des liens");
        (n.lines().collect(), l.lines().collect())
    }

    #[test]
    fn req142_ac1_late_seed_link_survives_a_big_earlier_seed() {
        use crate::model::EdgeKind::{Contains, Imports};
        let late = mk(NodeKind::Type, "z.Late", Some("tard"));
        let user = mk(NodeKind::Type, "z.User", Some("utilisateur"));
        let big = mk(NodeKind::Type, "a.Big", Some("gros"));
        let mut nodes = vec![late.clone(), user.clone(), big.clone()];
        let mut edges = Vec::new();
        for i in 0..80 {
            let m = mk(
                NodeKind::Method,
                &format!("a.Big.m{i}"),
                Some(&"r".repeat(120)),
            );
            edges.push(edge(&big, &m, Contains));
            nodes.push(m);
        }
        edges.push(edge(&user, &late, Imports));
        let ctx = build_context(
            &nodes,
            &edges,
            &[late.clone(), big.clone()],
            &AskLimits::default(),
        )
        .text;
        assert!(ctx.contains(&node_line(&user)), "{ctx}");
        assert!(
            ctx.contains("\n- type:z.User -imports-> type:z.Late\n"),
            "{ctx}"
        );
        assert!(ctx.chars().count() <= DEFAULT_ASK_MAX_CONTEXT_CHARS);
        let (n, _) = sections(&ctx);
        assert!(n.len() <= DEFAULT_ASK_MAX_NODES, "{}", n.len());
    }

    #[test]
    fn req142_ac2_context_is_cut_only_at_line_ends() {
        use crate::model::EdgeKind::Contains;
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seeds = Vec::new();
        for i in 0..20 {
            let s = mk(NodeKind::Type, &format!("p.S{i}"), Some(&"s".repeat(500)));
            for j in 0..30 {
                let v = mk(
                    NodeKind::Method,
                    &format!("p.S{i}.v{j}"),
                    Some(&"v".repeat(500)),
                );
                edges.push(edge(&s, &v, Contains));
                nodes.push(v);
            }
            seeds.push(s.clone());
            nodes.push(s);
        }
        let ctx = build_context(&nodes, &edges, &seeds, &AskLimits::default()).text;
        assert!(ctx.chars().count() <= DEFAULT_ASK_MAX_CONTEXT_CHARS);
        assert!(ctx.ends_with('\n'));
        assert_eq!(ctx.matches("Liens :\n").count(), 1);
        let valid_nodes: HashSet<String> = nodes.iter().map(node_line).collect();
        let valid_links: HashSet<String> = edges.iter().map(link_line).collect();
        let (n, l) = sections(&ctx);
        assert!(!n.is_empty() && !l.is_empty());
        for line in n {
            assert!(valid_nodes.contains(line), "ligne de nœud coupée : {line}");
        }
        for line in l {
            assert!(valid_links.contains(line), "ligne de lien coupée : {line}");
        }
    }

    #[test]
    fn req142_ac3_links_grouped_by_seed_priority() {
        use crate::model::EdgeKind::{Calls, References};
        let s: Vec<Node> = (1..=3)
            .map(|i| mk(NodeKind::Method, &format!("p.S{i}"), None))
            .collect();
        let n1 = mk(NodeKind::Method, "p.N1", None);
        let n2 = mk(NodeKind::Method, "p.N2", None);
        let edges = vec![
            edge(&s[2], &n1, Calls),
            edge(&s[1], &n1, Calls),
            edge(&s[0], &s[1], References),
            edge(&s[1], &n2, Calls),
            edge(&n1, &s[0], Calls),
            edge(&s[2], &s[0], Calls),
        ];
        let nodes = vec![
            n2.clone(),
            n1.clone(),
            s[2].clone(),
            s[1].clone(),
            s[0].clone(),
        ];
        let ctx = build_context(&nodes, &edges, &s, &AskLimits::default()).text;
        let want: Vec<String> = [2, 4, 5, 1, 3, 0]
            .iter()
            .map(|&i| link_line(&edges[i]))
            .collect();
        let (n, l) = sections(&ctx);
        assert_eq!(l, want.iter().map(String::as_str).collect::<Vec<_>>());
        let mut ids: Vec<&str> = n.iter().map(|x| x.split(" : ").next().unwrap()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(total, 5);
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn req142_ac4_neighbours_capped_so_that_seeds_keep_their_place() {
        use crate::model::EdgeKind::Calls;
        let seeds: Vec<Node> = (0..20)
            .map(|i| mk(NodeKind::Method, &format!("p.S{i}"), None))
            .collect();
        let mut nodes = seeds.clone();
        let mut edges = Vec::new();
        for i in 0..100 {
            let v = mk(NodeKind::Method, &format!("p.V{i:03}"), None);
            edges.push(edge(&seeds[0], &v, Calls));
            nodes.push(v);
        }
        let ctx = build_context(&nodes, &edges, &seeds, &AskLimits::default()).text;
        let (n, l) = sections(&ctx);
        assert_eq!(n.len(), 60);
        for s in &seeds {
            assert!(n.contains(&node_line(s).as_str()));
        }
        let want: Vec<String> = edges[..40].iter().map(link_line).collect();
        assert_eq!(l, want.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(n.contains(&node_line(&nodes[20 + 39]).as_str()));
        assert!(!n.contains(&node_line(&nodes[20 + 40]).as_str()));
    }

    #[test]
    fn req142_ac5_overlong_first_seed_line_is_cut_alone() {
        use crate::model::EdgeKind::Calls;
        let s = mk(NodeKind::Type, "p.S", Some(&"é".repeat(20_000)));
        let v = mk(NodeKind::Method, "p.V", None);
        let edges = vec![edge(&s, &v, Calls)];
        let ctx = build_context(
            &[s.clone(), v],
            &edges,
            std::slice::from_ref(&s),
            &AskLimits::default(),
        )
        .text;
        assert_eq!(ctx.chars().count(), DEFAULT_ASK_MAX_CONTEXT_CHARS);
        assert!(
            ctx.starts_with(&format!("Nœuds :\n- {} : ", s.id)),
            "{}",
            &ctx[..80]
        );
        assert!(ctx.ends_with("\nLiens :\n"));
        assert_eq!(ctx.lines().count(), 3);
    }

    fn limits_chars(max_context_chars: usize) -> AskLimits {
        AskLimits {
            max_context_chars,
            ..AskLimits::default()
        }
    }

    #[test]
    fn req163_ac1_context_chars_limit_is_a_parameter() {
        use crate::model::EdgeKind::Contains;
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seeds = Vec::new();
        for i in 0..20 {
            let s = mk(NodeKind::Type, &format!("p.S{i}"), Some(&"s".repeat(500)));
            for j in 0..30 {
                let v = mk(
                    NodeKind::Method,
                    &format!("p.S{i}.v{j}"),
                    Some(&"v".repeat(500)),
                );
                edges.push(edge(&s, &v, Contains));
                nodes.push(v);
            }
            seeds.push(s.clone());
            nodes.push(s);
        }
        let valid_nodes: HashSet<String> = nodes.iter().map(node_line).collect();
        let valid_links: HashSet<String> = edges.iter().map(link_line).collect();
        let at = |n: usize| build_context(&nodes, &edges, &seeds, &limits_chars(n)).text;
        for (limit, min) in [(2000, 1500), (30_000, 12_001)] {
            let ctx = at(limit);
            let len = ctx.chars().count();
            assert!(len <= limit && len >= min, "{limit} : {len}");
            assert!(ctx.ends_with('\n'));
            let (n, l) = sections(&ctx);
            assert!(n.iter().all(|x| valid_nodes.contains(*x)));
            assert!(l.iter().all(|x| valid_links.contains(*x)));
            assert!(!l.is_empty());
        }
        let default = build_context(&nodes, &edges, &seeds, &AskLimits::default()).text;
        assert_eq!(at(DEFAULT_ASK_MAX_CONTEXT_CHARS), default);
        assert!(default.chars().count() <= DEFAULT_ASK_MAX_CONTEXT_CHARS);
    }

    fn cap_graph() -> (Vec<Node>, Vec<Edge>, Vec<Node>) {
        use crate::model::EdgeKind::Calls;
        let seeds: Vec<Node> = (0..20)
            .map(|i| mk(NodeKind::Method, &format!("p.Seed{i:02}"), None))
            .collect();
        let mut nodes = seeds.clone();
        let mut edges = Vec::new();
        for i in 0..100 {
            let v = mk(NodeKind::Method, &format!("p.V{i:03}"), None);
            edges.push(edge(&seeds[0], &v, Calls));
            nodes.push(v);
        }
        (nodes, edges, seeds)
    }

    #[test]
    fn req163_ac2_max_nodes_and_max_seeds_are_parameters() {
        let (nodes, edges, _) = cap_graph();
        let run = |max_nodes: usize, max_seeds: usize| {
            let limits = AskLimits {
                max_nodes,
                max_seeds,
                ..AskLimits::default()
            };
            let seeds = find_seeds(&nodes, &[], &["seed".to_string()], &HashSet::new(), &limits);
            let ctx = build_context(&nodes, &edges, &seeds, &limits).text;
            let (n, l) = sections(&ctx);
            (seeds.len(), n.len(), l.len())
        };
        assert_eq!(run(60, 20), (20, 60, 40));
        assert_eq!(run(30, 20), (20, 30, 10));
        assert_eq!(run(10, 20), (10, 10, 0));
        assert_eq!(run(60, 5), (5, 60, 55));
        let ids = |max_nodes: usize, max_seeds: usize| -> Vec<String> {
            let limits = AskLimits {
                max_nodes,
                max_seeds,
                ..AskLimits::default()
            };
            find_seeds(&nodes, &[], &["seed".to_string()], &HashSet::new(), &limits)
                .into_iter()
                .map(|n| n.id)
                .collect()
        };
        let all = ids(60, 20);
        assert_eq!(all.len(), 20);
        assert_eq!(ids(10, 20), all[..10]);
        assert_eq!(ids(60, 5), all[..5]);
        // Une borne de contexte absurde ne fait pas paniquer.
        let seeds = &nodes[..1];
        let ctx = build_context(&nodes, &edges, seeds, &limits_chars(3)).text;
        assert!(ctx.starts_with("Nœuds :\n"));
    }

    #[test]
    fn req163_ac3_first_seed_line_is_cut_at_the_given_limit() {
        use crate::model::EdgeKind::Calls;
        let s = mk(NodeKind::Type, "p.S", Some(&"é".repeat(20_000)));
        let v = mk(NodeKind::Method, "p.V", None);
        let edges = vec![edge(&s, &v, Calls)];
        let ctx = build_context(
            &[s.clone(), v],
            &edges,
            std::slice::from_ref(&s),
            &limits_chars(ASK_MIN_CONTEXT_CHARS),
        )
        .text;
        assert_eq!(ctx.chars().count(), ASK_MIN_CONTEXT_CHARS);
        assert!(ctx.starts_with(&format!("Nœuds :\n- {} : ", s.id)));
        assert!(ctx.ends_with("\nLiens :\n"));
        assert_eq!(ctx.lines().count(), 3);
    }

    #[test]
    fn req160_ac4_legend_follows_node_and_link_lines() {
        use crate::model::{EdgeKind::Calls, SubKind};
        let a = Node::new(
            NodeKind::Type,
            "p.A",
            "A",
            Some("p/A.java"),
            Some(SubKind::Class),
            Some("Rôle de A."),
        );
        assert_eq!(
            node_line(&a),
            "- type:p.A : p.A (class, p/A.java) - Rôle de A."
        );
        assert!(a.id.starts_with("type:"));
        let c = Node::new(NodeKind::Concept, "x", "x", None, None, None);
        assert_eq!(node_line(&c), "- concept:x : x (concept)");
        let f = Node::new(NodeKind::Method, "p.A.f", "f", Some("p/A.java"), None, None);
        let g = Node::new(NodeKind::Method, "p.B.g", "g", Some("p/B.java"), None, None);
        let edges = vec![edge(&f, &g, Calls)];
        let ctx = build_context(
            &[f.clone(), g],
            &edges,
            std::slice::from_ref(&f),
            &AskLimits::default(),
        )
        .text;
        assert!(
            ctx.lines()
                .any(|l| l == "- method:p.A.f -calls-> method:p.B.g"),
            "{ctx}"
        );
        let prompt = crate::llm::ask::ask_system_prompt();
        assert!(prompt.contains("- <identifiant> : <nom qualifié> (<genre>, <fichier>) - <résumé>"));
        assert!(prompt.contains("- <source> -<genre>-> <cible>"));
    }

    /// Identifiants des lignes de nœuds d'un contexte, dans l'ordre.
    fn written_ids(ctx: &str) -> Vec<String> {
        let (n, _) = sections(ctx);
        n.iter()
            .map(|l| {
                l.split(" : ")
                    .next()
                    .unwrap()
                    .trim_start_matches("- ")
                    .to_string()
            })
            .collect()
    }

    fn shown_ids(c: &Context) -> Vec<String> {
        c.shown.iter().map(|n| n.id.clone()).collect()
    }

    #[test]
    fn req161_ac1_shown_nodes_are_the_written_node_lines() {
        use crate::model::EdgeKind::{Calls, Contains};
        // Un nœud de départ.
        let a = mk(NodeKind::Type, "p.A", Some("a"));
        let m = mk(NodeKind::Method, "p.A.m", None);
        let x = mk(NodeKind::Method, "p.X.x", None);
        let nodes = vec![x.clone(), m.clone(), a.clone()];
        let edges = vec![edge(&a, &m, Contains), edge(&x, &m, Calls)];
        let c = build_context(
            &nodes,
            &edges,
            std::slice::from_ref(&m),
            &AskLimits::default(),
        );
        assert_eq!(shown_ids(&c), written_ids(&c.text));
        assert_eq!(shown_ids(&c).len(), 3);
        // Plusieurs nœuds de départ, contexte plein coupé en fin de ligne.
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seeds = Vec::new();
        for i in 0..20 {
            let s = mk(NodeKind::Type, &format!("p.S{i}"), Some(&"s".repeat(500)));
            for j in 0..30 {
                let v = mk(
                    NodeKind::Method,
                    &format!("p.S{i}.v{j}"),
                    Some(&"v".repeat(500)),
                );
                edges.push(edge(&s, &v, Contains));
                nodes.push(v);
            }
            seeds.push(s.clone());
            nodes.push(s);
        }
        let c = build_context(&nodes, &edges, &seeds, &AskLimits::default());
        let shown = shown_ids(&c);
        assert_eq!(shown, written_ids(&c.text));
        assert!(shown.len() > 5);
        let (_, links) = sections(&c.text);
        assert!(!links.is_empty());
        for l in links {
            let (src, rest) = l.trim_start_matches("- ").split_once(" -").unwrap();
            let tgt = rest.split_once("-> ").unwrap().1;
            assert!(shown.iter().any(|i| i == src), "{l}");
            assert!(shown.iter().any(|i| i == tgt), "{l}");
        }
    }

    #[test]
    fn req161_ac1_cut_first_seed_is_shown_only_if_its_identifier_survives() {
        // Ligne coupée avec identifiant entier : montré.
        let s = mk(NodeKind::Type, "p.S", Some(&"é".repeat(20_000)));
        let c = build_context(
            std::slice::from_ref(&s),
            &[],
            std::slice::from_ref(&s),
            &AskLimits::default(),
        );
        assert_eq!(shown_ids(&c), vec![s.id.clone()]);
        // Identifiant plus long que la place disponible : aucun nœud montré.
        let long = mk(NodeKind::Type, &format!("p.{}", "L".repeat(13_000)), None);
        let c = build_context(
            std::slice::from_ref(&long),
            &[],
            std::slice::from_ref(&long),
            &AskLimits::default(),
        );
        assert!(c.shown.is_empty());
        assert_eq!(c.text.chars().count(), DEFAULT_ASK_MAX_CONTEXT_CHARS);
        // Identifiant entier mais sans le ` :` qui suit : aucun nœud montré.
        let room = DEFAULT_ASK_MAX_CONTEXT_CHARS - "Nœuds :\nLiens :\n".chars().count() - 1;
        let id_len = "type:".len();
        let name_len = room - "- ".len() - id_len;
        let edge_case = mk(NodeKind::Type, &"E".repeat(name_len), None);
        assert_eq!(format!("- {}", edge_case.id).chars().count(), room);
        let c = build_context(
            std::slice::from_ref(&edge_case),
            &[],
            std::slice::from_ref(&edge_case),
            &AskLimits::default(),
        );
        assert!(c.shown.is_empty());
    }

    fn resolve_cited(shown: &[&Node], cited: &[&str]) -> Vec<String> {
        let cited: Vec<String> = cited.iter().map(|c| c.to_string()).collect();
        retain_cited(shown, &cited)
            .into_iter()
            .map(|n| n.id)
            .collect()
    }

    #[test]
    fn req161_ac2_citations_are_resolved_among_shown_nodes() {
        let sa = mk(NodeKind::Method, "p.A.save", None);
        let sb = mk(NodeKind::Method, "p.B.save", None);
        let ta = mk(NodeKind::Type, "p.A", None);
        let shown = [&sa, &sb, &ta];
        for (name, want) in [
            ("method:p.A.save", vec!["method:p.A.save"]),
            ("p.A.save", vec!["method:p.A.save"]),
            ("A.save", vec!["method:p.A.save"]),
            ("save", vec![]),
            ("load", vec![]),
            ("method:q.C.load", vec![]),
            ("q.C.load", vec![]),
            ("Ghost", vec![]),
        ] {
            assert_eq!(resolve_cited(&shown, &[name]), want, "{name}");
        }
        // Unique dans le contexte, ambigu dans le graphe : retenu.
        assert_eq!(resolve_cited(&[&sa], &["save"]), vec!["method:p.A.save"],);
        // Ordre des citations, sans doublon.
        assert_eq!(
            resolve_cited(&shown, &["type:p.A", "p.A", "method:p.A.save"]),
            vec!["type:p.A", "method:p.A.save"]
        );
    }
}
