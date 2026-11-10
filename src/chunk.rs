//! Découpage des gros fichiers (REQ-007) : coupes sur fins de ligne, en-tête répété et fusion.

use std::collections::HashSet;

use crate::llm::enrich::{Concept, Enrichment};

/// Nombre de lignes de tête examinées pour trouver package, imports et type englobant.
const HEADER_SCAN_LINES: usize = 300;

/// Nombre maximal de lignes `import` recopiées dans l'en-tête répété (REQ-170).
const HEADER_MAX_IMPORTS: usize = 20;

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
    "export",
    "default",
    "declare",
];
const TYPE_KEYWORDS: &[&str] = &["class", "interface", "enum", "record", "object", "trait"];

/// Vrai si la ligne est une déclaration de type : modificateurs/annotations, mot-clé de type,
/// puis un mot qui commence par une lettre, `_` ou `$` (l'accolade n'est pas exigée, REQ-170).
fn is_type_declaration(line: &str) -> bool {
    let mut words = line.split_whitespace();
    while let Some(w) = words.next() {
        if MODIFIERS.contains(&w) || w.starts_with('@') {
            continue;
        }
        // `enum class`, `annotation class`, `@interface`
        if w == "annotation" || w == "fun" {
            continue;
        }
        return TYPE_KEYWORDS.contains(&w)
            && words
                .next()
                .and_then(|n| n.chars().next())
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$');
    }
    false
}

/// En-tête d'un fichier : lignes `package`, 20 premières lignes `import` et déclaration du type
/// englobant détectées en tête. Chaque ligne se termine par `\n` ; vide si rien n'est détecté.
pub fn header(text: &str) -> String {
    let mut out = String::new();
    let mut imports = 0usize;
    for line in text.lines().take(HEADER_SCAN_LINES) {
        let t = line.trim();
        if t.starts_with("package ") {
            out.push_str(line);
            out.push('\n');
        } else if t.starts_with("import ") {
            if imports < HEADER_MAX_IMPORTS {
                imports += 1;
                out.push_str(line);
                out.push('\n');
            }
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
    split_with(text, max, |_| (head.clone(), later_budget))
}

/// Comme `split_spans`, avec l'en-tête Markdown (REQ-170) pour un fichier Markdown : chaîne des
/// titres qui englobent le début du corps de chaque morceau k >= 2, bornée à `max_chars / 2`
/// caractères (titres externes retirés d'abord), déduite du budget du corps de ce morceau.
pub fn split_spans_for(path: &str, text: &str, max_chars: usize) -> Vec<(String, usize)> {
    if !crate::llm::enrich::is_markdown(path) {
        return split_spans(text, max_chars);
    }
    let max = max_chars.max(1);
    if text.chars().count() <= max {
        return vec![(text.to_string(), 0)];
    }
    let titles = crate::parse::markdown::heading_lines(text);
    let mut next = 0usize;
    // Pile des titres englobants : (niveau, ligne d'en-tête terminée par `\n`).
    let mut stack: Vec<(u8, String)> = Vec::new();
    split_with(text, max, |pos| {
        let mut pending = None;
        while let Some(&(level, start, setext)) = titles.get(next) {
            if start > pos {
                break;
            }
            next += 1;
            while stack.last().is_some_and(|(l, _)| *l >= level) {
                stack.pop();
            }
            let line = heading_header_line(text, level, start, setext);
            if start < pos {
                stack.push((level, line));
            } else {
                // Titre qui ouvre ce corps : absent de son en-tête, présent pour les suivants.
                pending = Some((level, line));
            }
        }
        let mut first = 0usize;
        let mut total: usize = stack.iter().map(|(_, l)| l.chars().count()).sum();
        while first < stack.len() && total > max / 2 {
            total -= stack[first].1.chars().count();
            first += 1;
        }
        let head: String = stack[first..].iter().map(|(_, l)| l.as_str()).collect();
        let budget = max.saturating_sub(total).max(1);
        stack.extend(pending);
        (head, budget)
    })
}

/// Ligne d'en-tête d'un titre : la ligne source d'un titre ATX, `#`×niveau + espace + première
/// ligne de texte pour un titre souligné ; terminée par `\n`.
fn heading_header_line(text: &str, level: u8, start: usize, setext: bool) -> String {
    let line = text[start..].lines().next().unwrap_or("");
    if setext {
        format!("{} {}\n", "#".repeat(level as usize), line.trim())
    } else {
        format!("{line}\n")
    }
}

/// Boucle de coupe commune : `head_at(pos)` donne l'en-tête et le budget du corps du morceau
/// (k >= 2) dont le corps commence à l'octet `pos` ; le premier morceau a `max` et pas d'en-tête.
fn split_with<F>(text: &str, max: usize, mut head_at: F) -> Vec<(String, usize)>
where
    F: FnMut(usize) -> (String, usize),
{
    let mut parts: Vec<(String, usize)> = Vec::new();
    let mut cur = String::new();
    let mut cur_start = 0usize;
    let mut cur_len = 0usize;
    let mut budget = max;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let n = line.chars().count();
        if cur_len > 0 && cur_len + n > budget {
            parts.push((std::mem::take(&mut cur), cur_start));
            cur_len = 0;
        }
        if cur.is_empty() {
            cur_start = pos;
            if !parts.is_empty() {
                let (head, b) = head_at(pos);
                cur.push_str(&head);
                budget = b;
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
/// nœud, concepts fusionnés par nom (premier résumé non vide, sections unies), rejets cumulés
/// sans doublon (égalité exacte, ordre de première apparition).
pub fn merge_enrichments(parts: Vec<Enrichment>) -> Enrichment {
    let mut out = Enrichment::default();
    let mut summarized: HashSet<String> = HashSet::new();
    let mut seen_rejected: HashSet<String> = HashSet::new();
    for p in parts {
        for name in p.served {
            if !out.served.contains(&name) {
                out.served.push(name);
            }
        }
        for (node, summary) in p.summaries {
            if summary.trim().is_empty() {
                continue;
            }
            if summarized.insert(node.clone()) {
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
        for r in p.rejected {
            if !seen_rejected.contains(&r) {
                seen_rejected.insert(r.clone());
                out.rejected.push(r);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enr(sum: &[(&str, &str)], concepts: Vec<Concept>) -> Enrichment {
        Enrichment {
            served: Vec::new(),
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

    /// `merge_enrichments` de la 0.24.0 (oracle).
    fn merge_oracle(parts: Vec<Enrichment>) -> Enrichment {
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

    #[test]
    fn req107_ac1_merge_enrichments_egale_l_oracle_sur_entrees_generees() {
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..300 {
            let mut parts = Vec::new();
            for _ in 0..(1 + next(8)) {
                let summaries = (0..next(8))
                    .map(|_| {
                        let text = match next(4) {
                            0 => String::new(),
                            1 => "  ".to_string(),
                            _ => format!("r{}", next(5)),
                        };
                        (format!("type:N{}", next(6)), text)
                    })
                    .collect();
                let concepts = (0..next(4))
                    .map(|_| {
                        let secs: Vec<String> = (0..next(3))
                            .map(|_| format!("section:g#{}", next(3)))
                            .collect();
                        let summary = (next(2) == 0).then(|| format!("d{}", next(3)));
                        Concept {
                            name: format!("c{}", next(3)),
                            summary,
                            sections: secs,
                        }
                    })
                    .collect();
                let rejected = (0..next(5)).map(|_| format!("rej {}", next(4))).collect();
                parts.push(Enrichment {
                    served: Vec::new(),
                    summaries,
                    concepts,
                    rejected,
                });
            }
            let want = merge_oracle(parts.clone());
            let got = merge_enrichments(parts);
            assert_eq!(got.summaries, want.summaries);
            assert_eq!(got.concepts, want.concepts);
            let mut seen = HashSet::new();
            let dedup: Vec<String> = want
                .rejected
                .into_iter()
                .filter(|r| seen.insert(r.clone()))
                .collect();
            assert_eq!(got.rejected, dedup);
        }
    }

    #[test]
    fn req107_ac2_merge_enrichments_vingt_mille_noeuds_en_temps_lineaire() {
        let parts: Vec<Enrichment> = (0..50)
            .map(|p| {
                let mut summaries: Vec<(String, String)> = (0..400)
                    .map(|i| (format!("type:P{p}N{i}"), format!("s{p}-{i}")))
                    .collect();
                summaries.extend((0..400).map(|i| (format!("type:P0N{i}"), "bis".to_string())));
                Enrichment {
                    served: Vec::new(),
                    summaries,
                    concepts: vec![],
                    rejected: vec![],
                }
            })
            .collect();
        let t = std::time::Instant::now();
        let m = merge_enrichments(parts);
        assert!(t.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(m.summaries.len(), 20_000);
        assert_eq!(m.summaries[0], ("type:P0N0".into(), "s0-0".into()));
        assert_eq!(m.summaries[400], ("type:P1N0".into(), "s1-0".into()));
        assert_eq!(m.summaries[19_999].0, "type:P49N399");
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

    #[test]
    fn req170_ac1_is_type_declaration() {
        for yes in [
            "export class Foo {",
            "export default class extends Base {",
            "export abstract class Repo<T> {",
            "declare class Foo {",
            "export interface Props {",
            "export enum Color {",
            "data class X(val a: Int)",
            "public class Foo",
            "object Main extends App",
            "sealed interface Shape",
            "enum class Color",
            "public final class A {",
            "@Service public class S {",
        ] {
            assert!(is_type_declaration(yes), "{yes}");
        }
        for no in [
            "record = repo.find(id);",
            "class",
            "object : Foo {",
            "// class Foo {",
            "* class Foo",
            "export default function f() {",
            "export const x = 1;",
            "import x from 'y';",
            "export type T = A;",
            "type T = { a: number }",
        ] {
            assert!(!is_type_declaration(no), "{no}");
        }
    }

    #[test]
    fn req170_ac2_header_limits_imports_to_twenty() {
        let mut t = String::from("package p;\n");
        for i in 0..60 {
            t.push_str(&format!("import a.I{i};\n"));
        }
        t.push_str("public class Big {\n  int x;\n}\n");
        let h = header(&t);
        let mut want = String::from("package p;\n");
        for i in 0..20 {
            want.push_str(&format!("import a.I{i};\n"));
        }
        want.push_str("public class Big {\n");
        assert_eq!(h, want);
        assert_eq!(h.lines().count(), 22);

        let ts = "import { A } from './a';\nexport class Foo {\n  x = 1;\n}\n";
        assert!(header(ts).contains("export class Foo {\n"));
        let kt = "package k\ndata class X(val a: Int)\n  fun f() {}\n";
        assert!(header(kt).ends_with("data class X(val a: Int)\n"));
    }

    fn md_doc() -> String {
        let mut t = String::from("# Guide\n\n## A\n\n### A.1\n\n");
        for i in 0..12 {
            t.push_str(&format!("mot{i} mot mot mot\n"));
        }
        t.push_str("\n## B\n\n```\n# pas un titre\n```\n\n");
        for i in 0..30 {
            t.push_str(&format!("texte b{i} texte texte\n"));
        }
        t.push_str("\nPartie\n======\n\n");
        for i in 0..20 {
            t.push_str(&format!("corps final {i} suite suite\n"));
        }
        t
    }

    #[test]
    fn req170_ac3_markdown_header_is_enclosing_headings() {
        let t = md_doc();
        let b_start = t.find("## B").unwrap();
        let a1 = t.find("### A.1").unwrap();
        let partie = t.find("Partie").unwrap();
        // Limite égale à la longueur du texte avant `## B` : B ouvre un morceau.
        let lim = t[..b_start].chars().count();
        let (mut in_a1, mut at_b, mut in_b, mut after_partie) = (false, false, false, false);
        for limit in [lim, 150] {
            let spans = split_spans_for("d.md", &t, limit);
            assert_eq!(spans[0], (t[..spans[1].1].to_string(), 0));
            for (k, (part, start)) in spans.iter().enumerate().skip(1) {
                let end = spans.get(k + 1).map_or(t.len(), |(_, s)| *s);
                let body = &t[*start..end];
                let head = &part[..part.len() - body.len()];
                assert!(part.ends_with(body));
                assert!(!head.contains("pas un titre"));
                if *start > a1 && *start < b_start {
                    assert_eq!(head, "# Guide\n## A\n### A.1\n");
                    in_a1 = true;
                } else if *start == b_start {
                    assert_eq!(head, "# Guide\n");
                    assert!(part.starts_with("# Guide\n## B"));
                    at_b = true;
                } else if *start > b_start && *start < partie {
                    assert_eq!(head, "# Guide\n## B\n");
                    in_b = true;
                } else if *start > partie {
                    assert_eq!(head, "# Partie\n");
                    after_partie = true;
                }
            }
        }
        assert!(in_a1 && at_b && in_b && after_partie);
        // Chemin non Markdown : identique à `split_spans`.
        assert_eq!(split_spans_for("d.txt", &t, 150), split_spans(&t, 150));
        assert_eq!(
            split_spans_for("d.md", "court\n", 150),
            vec![("court\n".to_string(), 0)]
        );
    }

    #[test]
    fn req170_ac3_titre_dans_citation_ou_liste_reste_atx() {
        let t = "# Un\n\n> ## Cité\n\ntexte\n";
        let h = crate::parse::markdown::heading_lines(t);
        assert_eq!(h.len(), 2);
        assert!(!h[1].2);
        assert!(h[1].1 == t.find("> ##").unwrap());
    }

    #[test]
    fn req170_ac4_markdown_budget_property() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for _ in 0..200 {
            let max = 40 + next(361) as usize;
            let mut t = String::new();
            let mut titles: Vec<(usize, usize, String)> = Vec::new();
            for _ in 0..(5 + next(40)) {
                if next(3) == 0 {
                    let lvl = 1 + next(6) as usize;
                    let line = format!("{} T{}\n", "#".repeat(lvl), next(100));
                    titles.push((lvl, t.len(), line.clone()));
                    t.push_str(&line);
                } else {
                    let len = 1 + next((max / 2 - 1) as u64) as usize;
                    t.push_str(&"x".repeat(len.min(max / 2 - 1).max(1)));
                    t.push('\n');
                }
            }
            let spans = split_spans_for("g.md", &t, max);
            assert_eq!(spans[0].1, 0);
            let mut rebuilt = String::new();
            let mut prev = 0usize;
            for (k, (part, start)) in spans.iter().enumerate() {
                let end = spans.get(k + 1).map_or(t.len(), |(_, s)| *s);
                let body = &t[*start..end];
                assert!(part.ends_with(body), "{part:?} / {body:?}");
                let head = &part[..part.len() - body.len()];
                assert!(part.chars().count() <= max, "{part:?} max {max}");
                assert!(head.chars().count() <= max / 2);
                // Pile naïve indépendante : titres avant `start`, puis celui qui y commence.
                let mut naive: Vec<(usize, &str)> = Vec::new();
                for (lvl, at, line) in &titles {
                    if *at > *start {
                        break;
                    }
                    while naive.last().is_some_and(|(l, _)| l >= lvl) {
                        naive.pop();
                    }
                    if *at < *start || k == 0 {
                        naive.push((*lvl, line));
                    }
                }
                if k > 0 {
                    let mut want = String::new();
                    for i in 0..=naive.len() {
                        let cand: String = naive[i..].iter().map(|(_, l)| *l).collect();
                        if cand.chars().count() <= max / 2 {
                            want = cand;
                            break;
                        }
                    }
                    assert_eq!(head, want, "max {max} start {start}");
                }
                assert!(k == 0 && head.is_empty() || k > 0);
                if k > 0 {
                    assert!(*start > prev);
                }
                prev = *start;
                rebuilt.push_str(body);
            }
            assert_eq!(rebuilt, t);
        }
    }

    #[test]
    fn req170_ac4_outermost_titles_dropped_first() {
        let t = format!(
            "# Un\n## Deux\n### Trois\n{}\n{}\n",
            "a".repeat(20),
            "b".repeat(20)
        );
        // max 40 : plafond 20 ; chaîne complète = 5+8+9 = 22 > 20, le titre externe saute.
        let spans = split_spans_for("g.md", &t, 40);
        let second = &spans[1].0;
        assert!(second.starts_with("## Deux\n### Trois\n"), "{second:?}");
    }
}
