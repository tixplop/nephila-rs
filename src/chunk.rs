//! Découpage des gros fichiers (REQ-007) : coupes sur fins de ligne, en-tête répété et fusion.

use std::collections::HashSet;

use crate::llm::Extraction;

/// Nombre de lignes de tête examinées pour trouver package, imports et type englobant.
const HEADER_SCAN_LINES: usize = 300;

const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "abstract",
    "final",
    "static",
    "sealed",
    "open",
    "data",
    "case",
    "inner",
    "value",
    "strictfp",
    "non-sealed",
];
const TYPE_KEYWORDS: &[&str] = &["class", "interface", "enum", "record", "object", "trait"];

/// Vrai si la ligne est une déclaration de type : modificateurs/annotations, mot-clé, puis `{`.
fn is_type_declaration(line: &str) -> bool {
    if !line.contains('{') {
        return false;
    }
    let mut words = line.split_whitespace();
    while let Some(w) = words.next() {
        if MODIFIERS.contains(&w) || w.starts_with('@') {
            continue;
        }
        // `enum class`, `annotation class`, `@interface`
        if w == "annotation" || w == "fun" {
            continue;
        }
        return TYPE_KEYWORDS.contains(&w) && words.next().is_some();
    }
    false
}

/// En-tête d'un fichier : lignes `package`, `import` et déclaration du type englobant détectées
/// en tête. Chaque ligne se termine par `\n` ; vide si rien n'est détecté.
pub fn header(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines().take(HEADER_SCAN_LINES) {
        let t = line.trim();
        if t.starts_with("package ") || t.starts_with("import ") {
            out.push_str(line);
            out.push('\n');
        } else if is_type_declaration(t) {
            out.push_str(line);
            out.push('\n');
            break;
        }
    }
    out
}

/// Découpe `text` en morceaux d'au plus `max_chars` caractères (coupes sur fins de ligne). Les
/// morceaux suivant le premier sont précédés de l'en-tête. Une ligne plus longue que la limite
/// forme un morceau à elle seule. Un texte sous la limite est renvoyé tel quel.
pub fn split(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(1);
    if text.chars().count() <= max {
        return vec![text.to_string()];
    }
    let head = header(text);
    let head_len = head.chars().count();
    // L'en-tête ne doit pas manger tout le budget : au moins la moitié reste pour le corps.
    let later_budget = max.saturating_sub(head_len).max(max / 2).max(1);

    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for line in text.split_inclusive('\n') {
        let n = line.chars().count();
        let budget = if parts.is_empty() { max } else { later_budget };
        if cur_len > 0 && cur_len + n > budget {
            parts.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur.is_empty() && !parts.is_empty() {
            cur.push_str(&head);
        }
        cur.push_str(line);
        cur_len += n;
    }
    if cur_len > 0 {
        parts.push(cur);
    }
    parts
}

/// Fusionne les extractions des morceaux d'un même fichier : nœuds de même identifiant fusionnés
/// (les champs absents sont complétés), références dédupliquées, premier contexte conservé.
pub fn merge(parts: Vec<Extraction>) -> Extraction {
    let mut out = Extraction::default();
    let mut refs: HashSet<(String, String, String)> = HashSet::new();
    for p in parts {
        if out.context.is_none() {
            out.context = p.context;
        }
        for d in p.declares {
            let id = d.id();
            match out.declares.iter_mut().find(|e| e.id() == id) {
                Some(e) => {
                    if e.subkind.is_none() {
                        e.subkind = d.subkind;
                    }
                    if e.summary.is_none() {
                        e.summary = d.summary;
                    }
                }
                None => out.declares.push(d),
            }
        }
        for r in p.references {
            if refs.insert((r.source.clone(), r.kind.to_string(), r.target.clone())) {
                out.references.push(r);
            }
        }
    }
    out
}
