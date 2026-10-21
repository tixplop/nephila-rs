//! Découpage des gros fichiers (REQ-007) : coupes sur fins de ligne, en-tête répété et fusion.

use crate::llm::enrich::{Concept, Enrichment};

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
    split_spans(text, max_chars)
        .into_iter()
        .map(|(part, _)| part)
        .collect()
}

/// Comme `split`, avec pour chaque morceau l'octet de début de son corps dans `text` (0 pour le
/// premier ; l'en-tête répété n'en fait pas partie). Le corps du morceau k s'étend jusqu'au début
/// du corps du suivant (DEC-061).
pub fn split_spans(text: &str, max_chars: usize) -> Vec<(String, usize)> {
    let max = max_chars.max(1);
    if text.chars().count() <= max {
        return vec![(text.to_string(), 0)];
    }
    let head = header(text);
    let head_len = head.chars().count();
    // L'en-tête ne doit pas manger tout le budget : au moins la moitié reste pour le corps.
    let later_budget = max.saturating_sub(head_len).max(max / 2).max(1);

    let mut parts: Vec<(String, usize)> = Vec::new();
    let mut cur = String::new();
    let mut cur_start = 0usize;
    let mut cur_len = 0usize;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let n = line.chars().count();
        let budget = if parts.is_empty() { max } else { later_budget };
        if cur_len > 0 && cur_len + n > budget {
            parts.push((std::mem::take(&mut cur), cur_start));
            cur_len = 0;
        }
        if cur.is_empty() {
            cur_start = pos;
            if !parts.is_empty() {
                cur.push_str(&head);
            }
        }
        cur.push_str(line);
        cur_len += n;
        pos += line.len();
    }
    if cur_len > 0 {
        parts.push((cur, cur_start));
    }
    parts
}

/// Fusionne les enrichissements des morceaux d'un même fichier : premier résumé non vide par
/// nœud, concepts fusionnés par nom (premier résumé non vide, sections unies), rejets cumulés.
pub fn merge_enrichments(parts: Vec<Enrichment>) -> Enrichment {
    let mut out = Enrichment::default();
    for p in parts {
        for (node, summary) in p.summaries {
            if summary.trim().is_empty() {
                continue;
            }
            if !out.summaries.iter().any(|(n, _)| *n == node) {
                out.summaries.push((node, summary));
            }
        }
        for c in p.concepts {
            match out.concepts.iter_mut().find(|e| e.name == c.name) {
                Some(e) => {
                    if e.summary.is_none() {
                        e.summary = c.summary;
                    }
                    for s in c.sections {
                        if !e.sections.contains(&s) {
                            e.sections.push(s);
                        }
                    }
                }
                None => out.concepts.push(Concept {
                    name: c.name,
                    summary: c.summary,
                    sections: c.sections,
                }),
            }
        }
        out.rejected.extend(p.rejected);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enr(sum: &[(&str, &str)], concepts: Vec<Concept>) -> Enrichment {
        Enrichment {
            summaries: sum
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            concepts,
            rejected: vec![],
        }
    }

    fn concept(name: &str, summary: Option<&str>, sections: &[&str]) -> Concept {
        Concept {
            name: name.into(),
            summary: summary.map(String::from),
            sections: sections.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn req007_merge_enrichments_first_summary_and_union_of_sections() {
        let a = enr(
            &[("type:A", "premier")],
            vec![concept("facturation", None, &["section:g#a"])],
        );
        let b = enr(
            &[("type:A", "second"), ("type:B", "bb")],
            vec![concept(
                "facturation",
                Some("def"),
                &["section:g#a", "section:g#b"],
            )],
        );
        let m = merge_enrichments(vec![a, b]);
        assert_eq!(
            m.summaries,
            vec![
                ("type:A".into(), "premier".into()),
                ("type:B".into(), "bb".into())
            ]
        );
        assert_eq!(m.concepts.len(), 1);
        assert_eq!(m.concepts[0].summary.as_deref(), Some("def"));
        assert_eq!(m.concepts[0].sections, ["section:g#a", "section:g#b"]);
    }

    #[test]
    fn split_spans_corps_et_concatenation() {
        let mut text = String::from("package p;\nimport x.Y;\npublic class A {\n");
        for i in 0..40 {
            text.push_str(&format!("  void m{i}() {{}}\n"));
        }
        text.push_str("}\n");
        let spans = split_spans(&text, 200);
        assert!(spans.len() > 2);
        assert_eq!(spans[0].1, 0);
        let head = header(&text);
        assert!(!head.is_empty());
        let mut rebuilt = String::new();
        for (k, (part, start)) in spans.iter().enumerate() {
            let end = spans.get(k + 1).map_or(text.len(), |(_, s)| *s);
            let body = &text[*start..end];
            if k == 0 {
                assert_eq!(part, body);
            } else {
                assert_eq!(*part, format!("{head}{body}"));
            }
            rebuilt.push_str(body);
        }
        assert_eq!(rebuilt, text);
        let parts: Vec<String> = spans.into_iter().map(|(p, _)| p).collect();
        assert_eq!(split(&text, 200), parts);
        assert_eq!(
            split_spans("court\n", 200),
            vec![("court\n".to_string(), 0)]
        );
    }
}
