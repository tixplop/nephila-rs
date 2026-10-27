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

/// Clé de classement d'un nœud pour un terme : (classe, fréquence, rang, position du terme).
type SeedKey = (u8, usize, u8, usize);

/// Nœuds de départ : les nœuds qui correspondent à au moins un terme (noms qualifiés cités puis
/// mots-clés), triés par leur meilleure clé, puis nom qualifié et identifiant ; les 20 premiers.
/// La recherche travaille sur des références ; seuls les nœuds retenus sont copiés.
fn find_seeds(nodes: &[Node], qualified: &[String], words: &[String]) -> Vec<Node> {
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
                (if r == 0 { 1 } else { 2 }, freq, r, qualified.len() + k),
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
        .take(MAX_SEEDS)
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
    let seeds = find_seeds(&nodes, &qualified_terms(question), &keywords(question));
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
        writeln!(out, "{}", crate::escape::ask_answer(&answer))?;
        writeln!(out, "Nœuds cités :")?;
        for n in &cited {
            let l = format!("- {}", super::describe(n));
            writeln!(out, "{}", crate::escape::data(&l))?;
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
        seeds.truncate(MAX_SEEDS);
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
        find_seeds(nodes, &qualified_terms(question), &keywords(question))
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
            find_seeds(&nodes, &[term.to_string()], &[])
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
        find_seeds(nodes, &[], &w)
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
        let got: Vec<String> = find_seeds(&nodes, &["bar.baz".to_string()], &["foo".to_string()])
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

    /// Oracle naïf d'AC-141-5, écrit depuis la définition.
    fn naive_seeds(nodes: &[Node], quals: &[String], words: &[String]) -> Vec<String> {
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
                    hit(n, t).map(|r| (if r == 0 { 1 } else { 2 }, freq, r, pos))
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
            .take(MAX_SEEDS)
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
            let want = naive_seeds(&nodes, &quals, &words);
            capped |= want.len() == MAX_SEEDS;
            let got: Vec<String> = find_seeds(&nodes, &quals, &words)
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
            truncated |= old.len() == MAX_SEEDS;
            assert_eq!(old, find_seeds(&nodes, &[], &words), "mots {words:?}");
        }
        assert!(truncated, "le plafond de 20 nœuds n'est jamais atteint");
    }
}
