//! Découpage des gros fichiers (REQ-007) : coupes sur fins de ligne, reculées à une ligne sûre
//! (REQ-288), en-tête répété et fusion.

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

/// Découpe `text` en morceaux d'au plus `max_chars` caractères (coupes sur fins de ligne, reculées
/// à une ligne sûre, REQ-288). Les morceaux suivant le premier sont précédés de l'en-tête. Une ligne plus longue que la limite
/// forme un morceau à elle seule. Un texte sous la limite est renvoyé tel quel.
pub fn split(text: &str, max_chars: usize) -> Vec<String> {
    split_spans(text, max_chars)
        .into_iter()
        .map(|(part, _)| part)
        .collect()
}

/// Comme `split`, avec pour chaque morceau l'octet de début de son corps dans `text` (0 pour le
/// premier ; l'en-tête répété n'en fait pas partie). Le corps du morceau k s'étend jusqu'au début
/// du corps du suivant (DEC-061). Coupes sur fins de ligne, reculées à une ligne sûre (REQ-288).
pub fn split_spans(text: &str, max_chars: usize) -> Vec<(String, usize)> {
    let max = max_chars.max(1);
    if text.chars().count() <= max {
        return vec![(text.to_string(), 0)];
    }
    let head = header(text);
    let head_len = head.chars().count();
    // L'en-tête ne doit pas manger tout le budget : au moins la moitié reste pour le corps.
    let later_budget = max.saturating_sub(head_len).max(max / 2).max(1);
    split_with(text, max, &safe_starts_code(text), |_| {
        (head.clone(), later_budget)
    })
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
    let safe: Vec<usize> = titles
        .iter()
        .filter(|(level, _, _)| *level <= 2)
        .map(|&(_, start, _)| start)
        .collect();
    split_with(text, max, &safe, |pos| {
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

/// Vrai si la ligne ne contient que des blancs (espace, tabulation, fin de ligne).
fn is_blank(line: &str) -> bool {
    line.trim_matches([' ', '\t', '\r', '\n']).is_empty()
}

/// Nombre de blancs (espace ou tabulation) en tête de ligne.
fn indentation(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Débuts de ligne sûrs d'un fichier non Markdown (REQ-288) : m = indentation des membres, celle
/// de la première ligne non vide qui suit la déclaration de type de `header` (0 sinon). Une ligne i
/// est sûre si la ligne i - 1 est vide et la ligne i non vide d'indentation <= m, ou si la ligne
/// i - 1 commence par `}` (blancs de tête exceptés) avec une indentation <= m.
fn safe_starts_code(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let decl = lines.iter().take(HEADER_SCAN_LINES).position(|l| {
        let t = l.trim();
        !t.starts_with("package ") && !t.starts_with("import ") && is_type_declaration(t)
    });
    let m = decl
        .and_then(|d| lines[d + 1..].iter().find(|l| !is_blank(l)))
        .map_or(0, |l| indentation(l));
    let mut out = Vec::new();
    let mut pos = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let prev = lines[i - 1];
            let blank_then_member = is_blank(prev) && !is_blank(line) && indentation(line) <= m;
            let closing = prev.trim_start().starts_with('}') && indentation(prev) <= m;
            if blank_then_member || closing {
                out.push(pos);
            }
        }
        pos += line.len();
    }
    out
}

/// Empreinte d'une position de coupe : BLAKE3 du texte de `p` à la fin de la deuxième ligne
/// (ou du texte), 8 premiers octets lus petit-boutiste. Elle ne dépend que des deux lignes.
fn anchor_weight(text: &str, p: usize) -> u64 {
    let rest = &text[p..];
    let end = rest
        .match_indices('\n')
        .nth(1)
        .map_or(rest.len(), |(i, _)| i + 1);
    let h = blake3::hash(&rest.as_bytes()[..end]);
    u64::from_le_bytes(h.as_bytes()[..8].try_into().expect("8 octets"))
}

/// Boucle de coupe commune : `head_at(pos)` donne l'en-tête et le budget du corps du morceau
/// (k >= 2) dont le corps commence à l'octet `pos` ; le premier morceau a `max` et pas d'en-tête.
/// `safe` (croissant) liste les débuts de ligne sûrs : au dépassement, la coupe recule à la
/// position sûre de plus petite empreinte (`anchor_weight`, la première à égalité) qui reporte
/// au plus 15 % du budget au morceau suivant (REQ-288, REQ-294).
fn split_with<F>(text: &str, max: usize, safe: &[usize], mut head_at: F) -> Vec<(String, usize)>
where
    F: FnMut(usize) -> (String, usize),
{
    let mut parts: Vec<(String, usize)> = Vec::new();
    let mut head = String::new();
    let mut cur_start = 0usize;
    let mut cur_len = 0usize;
    let mut budget = max;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let n = line.chars().count();
        while cur_len > 0 && cur_len + n > budget {
            let window = budget * 15 / 100;
            let hi = safe.partition_point(|&p| p <= pos);
            let lo = safe.partition_point(|&p| p <= cur_start);
            // Début d'octet de la fenêtre : les `window` derniers caractères avant `pos`.
            let from = text[..pos]
                .char_indices()
                .rev()
                .nth(window.saturating_sub(1))
                .map_or(0, |(i, _)| i);
            let from = if window == 0 { pos } else { from };
            let lo = lo + safe[lo..hi].partition_point(|&p| p < from);
            let cut = safe[lo..hi]
                .iter()
                .min_by_key(|&&p| anchor_weight(text, p))
                .copied()
                .unwrap_or(pos);
            parts.push((format!("{head}{}", &text[cur_start..cut]), cur_start));
            (head, budget) = head_at(cut);
            cur_start = cut;
            cur_len = text[cut..pos].chars().count();
        }
        if cur_len == 0 {
            cur_start = pos;
        }
        cur_len += n;
        pos += line.len();
    }
    if cur_len > 0 {
        parts.push((format!("{head}{}", &text[cur_start..]), cur_start));
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

    // ---- REQ-288 : coupe reculée à une ligne sûre ----

    /// Boucle de coupe de la 0.28.0 recopiée telle quelle (oracle d'AC-288-3).
    fn split_oracle<F>(text: &str, max: usize, mut head_at: F) -> Vec<(String, usize)>
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

    fn oracle_spans(text: &str, max_chars: usize) -> Vec<(String, usize)> {
        let max = max_chars.max(1);
        if text.chars().count() <= max {
            return vec![(text.to_string(), 0)];
        }
        let head = header(text);
        let head_len = head.chars().count();
        let later = max.saturating_sub(head_len).max(max / 2).max(1);
        split_oracle(text, max, |_| (head.clone(), later))
    }

    /// Limite telle que la ligne commençant à `at` dépasse le budget du premier morceau.
    fn limit_before(text: &str, at: usize) -> usize {
        text[..at].chars().count() + 1
    }

    #[test]
    fn req288_ac1_methode_commencee_dans_la_fenetre_part_en_entier() {
        let mut t = String::from("public class A {\n    int pad0;\n");
        for i in 1..9 {
            t.push_str(&format!("    void p{i}() {{ a(); }}\n"));
        }
        t.push_str("    void m1() {\n        a();\n    }\n");
        t.push_str("    void m2() {\n        x();\n\n        if (y) {\n            z();\n        }\n        w();\n    }\n}\n");
        let m2 = t.find("    void m2").unwrap();
        let x = t.find("        x();").unwrap();
        let end_m2 = t.find("    }\n}\n").unwrap() + "    }\n".len();
        let spans = split_spans(&t, limit_before(&t, x));
        // La coupe d'aujourd'hui tombe avant `x();`, dans m2.
        assert!(oracle_spans(&t, limit_before(&t, x))
            .iter()
            .any(|(_, s)| *s == x));
        assert_eq!(spans[1].1, m2, "{spans:?}");
        assert!(spans[1].1 <= m2 && spans.get(2).is_none_or(|(_, s)| *s >= end_m2));
        let blank = t.find("\n\n").unwrap() + 1;
        let brace8 = t.find("        }\n").unwrap();
        for (_, s) in &spans {
            assert!(*s != blank && *s != brace8 && *s != x);
        }
        // Sans le `}` de m1, mais avec une ligne vide avant m2 : le morceau commence à m2.
        let mut u = String::from("public class A {\n");
        for i in 0..9 {
            u.push_str(&format!("    int f{i} = 1;\n"));
        }
        u.push_str("\n    void m2() {\n        x();\n        y();\n    }\n}\n");
        let m2 = u.find("    void m2").unwrap();
        let x = u.find("        x();").unwrap();
        let spans = split_spans(&u, limit_before(&u, x));
        assert_eq!(spans[1].1, m2, "{spans:?}");
    }

    #[test]
    fn req288_ac2_markdown_coupe_sur_titre_de_niveau_1_ou_2() {
        let para = |n: usize, c: &str| {
            (0..n)
                .map(|_| format!("{}\n", c.repeat(16)))
                .collect::<String>()
        };
        let mut t = String::from("# Guide\n## A\n");
        t += &para(20, "a");
        let b = t.len();
        t += "## B\n```\n# pas un titre\n```\n";
        t += &para(1, "p");
        let cut = t.len();
        t += &para(10, "q");
        let spans = split_spans_for("d.md", &t, limit_before(&t, cut) + 4);
        assert_eq!(spans[1].1, b, "{spans:?}");
        assert!(
            spans[1].0.starts_with("# Guide\n## B\n"),
            "{:?}",
            spans[1].0
        );

        // Seuls `### C` ou `# pas un titre` dans la fenêtre : coupe d'aujourd'hui.
        let mut u = para(24, "a");
        u += "### C\n";
        u += &para(12, "p");
        let title = u.find("### C").unwrap();
        let cut = title + 6 + 17 * 2;
        let lim = limit_before(&u, cut) + 4;
        assert!(u[title..cut].chars().count() <= lim * 15 / 100);
        assert_eq!(split_oracle(&u, lim, |_| (String::new(), lim))[1].1, cut);
        assert_eq!(
            split_spans_for("d.md", &u, lim)[1].1,
            split_oracle(&u, lim, |_| (String::new(), lim))[1].1
        );
        let mut v = para(24, "a");
        v += "```\n# pas un titre\n```\n";
        v += &para(12, "p");
        let title = v.find("# pas un titre").unwrap();
        let cut = title + 15 + 4 + 17;
        let lim = limit_before(&v, cut) + 4;
        assert!(v[title..cut].chars().count() <= lim * 15 / 100);
        assert_eq!(split_oracle(&v, lim, |_| (String::new(), lim))[1].1, cut);
        assert_eq!(
            split_spans_for("d.md", &v, lim)[1].1,
            split_oracle(&v, lim, |_| (String::new(), lim))[1].1
        );

        // Titre souligné de `-` : le morceau commence à sa ligne de texte.
        let mut w = String::from("# Guide\n");
        w += &para(24, "a");
        w += "\n";
        let sub = w.len();
        w += "Sous titre\n----------\n";
        w += &para(1, "p");
        let cut = w.len();
        w += &para(10, "q");
        let spans = split_spans_for("d.md", &w, limit_before(&w, cut) + 4);
        assert_eq!(spans[1].1, sub, "{spans:?}");
    }

    #[test]
    fn req288_ac3_repli_identique_a_la_boucle_de_la_0_28_0() {
        // Aucune ligne sûre : méthodes d'une ligne.
        let mut a = String::from("public class A {\n");
        for i in 0..40 {
            a.push_str(&format!("    void m{i}() {{}}\n"));
        }
        a.push_str("}\n");
        // Membre plus long que la fenêtre.
        let mut b = String::from("public class B {\n    void a() {}\n    }\n");
        b.push_str("    void long() {\n");
        for i in 0..30 {
            b.push_str(&format!("        s{i}();\n"));
        }
        b.push_str("    }\n}\n");
        // Lignes vides et accolades partout, petits budgets.
        let c = "class C {\n    int a;\n\n    int b;\n    }\n    int c;\n\n    int d;\n}\n";
        for (t, limits) in [
            (&a, vec![60usize, 200, 1000]),
            (&b, vec![120, 200, 300]),
            (&(c.to_string()), vec![1, 2, 3, 4, 5, 6]),
        ] {
            for lim in limits {
                assert_eq!(split_spans(t, lim), oracle_spans(t, lim), "{lim}");
                assert_eq!(split_spans_for("x.java", t, lim), oracle_spans(t, lim));
            }
        }
        let short = "court\n";
        assert_eq!(split_spans(short, 100), oracle_spans(short, 100));
        assert_eq!(
            split_spans_for("x.md", short, 100),
            oracle_spans(short, 100)
        );
        // Markdown sans titre de niveau 1 ou 2 ni en-tête.
        let mut m = String::new();
        for i in 0..30 {
            m.push_str(&format!("ligne {i} de texte\n"));
        }
        m.push('\n');
        for i in 0..30 {
            m.push_str(&format!("suite {i} de texte\n"));
        }
        for lim in [3usize, 6, 50, 200] {
            assert_eq!(
                split_spans_for("x.md", &m, lim),
                split_oracle(&m, lim, |_| (String::new(), lim)),
                "{lim}"
            );
        }
    }

    fn indent(line: &str) -> usize {
        line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
    }

    /// Positions sûres (code), recalculées ligne par ligne, `m` = indentation des membres.
    fn naive_safe_code(text: &str, m: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        let mut prev: Option<&str> = None;
        for line in text.split_inclusive('\n') {
            if let Some(p) = prev {
                let blank = |l: &str| l.trim().is_empty();
                let a = blank(p) && !blank(line) && indent(line) <= m;
                let b = p.trim_start().starts_with('}') && indent(p) <= m;
                if a || b {
                    out.push(pos);
                }
            }
            prev = Some(line);
            pos += line.len();
        }
        out
    }

    /// Empreinte d'une position : BLAKE3 des deux lignes qui commencent là, 8 premiers octets
    /// petit-boutistes (règle naïve du test, indépendante du code).
    fn naive_weight(text: &str, p: usize) -> u64 {
        let rest = &text[p..];
        let end = rest
            .match_indices('\n')
            .nth(1)
            .map_or(rest.len(), |(i, _)| i + 1);
        let h = blake3::hash(&rest.as_bytes()[..end]);
        u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
    }

    #[test]
    fn req288_ac4_proprietes_et_regle_naive() {
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for case in 0..300 {
            let md = case % 2 == 1;
            let max = 20 + next(381) as usize;
            let mut t = String::new();
            // (niveau, début, ligne d'en-tête)
            let mut titles: Vec<(usize, usize, String)> = Vec::new();
            if md {
                for _ in 0..(8 + next(40)) {
                    match next(6) {
                        0 | 1 => {
                            let lvl = 1 + next(6) as usize;
                            let line = format!("{} T{}\n", "#".repeat(lvl), next(100));
                            titles.push((lvl, t.len(), line.clone()));
                            t.push_str(&line);
                        }
                        2 => {
                            let lvl = 1 + next(2) as usize;
                            let text = format!("S{}", next(100));
                            titles.push((
                                lvl,
                                t.len() + 1,
                                format!("{} {text}\n", "#".repeat(lvl)),
                            ));
                            t.push_str(&format!(
                                "\n{text}\n{}\n",
                                if lvl == 1 { "====" } else { "----" }
                            ));
                            // la ligne vide ajoutée avant est hors titre
                            let last = titles.last_mut().unwrap();
                            last.1 = t.len() - text.len() - 1 - 5;
                        }
                        3 => t.push_str("```\n# faux\n```\n"),
                        _ => {
                            let len = 1 + next((max / 2 - 1) as u64) as usize;
                            t.push_str(&"x".repeat(len));
                            t.push('\n');
                        }
                    }
                }
            } else {
                t.push_str("class A {\n    int a;\n");
                for _ in 0..(8 + next(60)) {
                    match next(6) {
                        0 => t.push('\n'),
                        1 => t.push_str(&format!("{}}}\n", " ".repeat(4 * next(3) as usize))),
                        _ => {
                            let len = 1 + next(max as u64 * 3 / 2) as usize;
                            t.push_str(&" ".repeat(4 * (1 + next(2) as usize)));
                            t.push_str(&"y".repeat(len));
                            t.push('\n');
                        }
                    }
                }
            }
            let spans = if md {
                split_spans_for("g.md", &t, max)
            } else {
                split_spans(&t, max)
            };
            assert_eq!(spans[0].1, 0);
            let head_code = header(&t);
            let m = if md { 0 } else { 4 };
            let safe: Vec<usize> = if md {
                titles
                    .iter()
                    .filter(|(l, _, _)| *l <= 2)
                    .map(|(_, s, _)| *s)
                    .collect()
            } else {
                naive_safe_code(&t, m)
            };
            let mut rebuilt = String::new();
            for (k, (part, start)) in spans.iter().enumerate() {
                let end = spans.get(k + 1).map_or(t.len(), |(_, s)| *s);
                let body = &t[*start..end];
                assert!(!body.is_empty());
                assert!(part.ends_with(body));
                let head = &part[..part.len() - body.len()];
                if k > 0 {
                    assert!(*start > spans[k - 1].1);
                }
                assert!(
                    part.chars().count() <= max || body.split_inclusive('\n').count() == 1,
                    "case {case} part {part:?} max {max}"
                );
                if md {
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
                        let want = (0..=naive.len())
                            .map(|i| naive[i..].iter().map(|(_, l)| *l).collect::<String>())
                            .find(|c| c.chars().count() <= max / 2)
                            .unwrap();
                        assert_eq!(head, want, "case {case}");
                    }
                } else if k > 0 {
                    assert_eq!(head, head_code);
                }
                // Règle naïve : coupe du morceau k à partir du corps du morceau k - 1 (k >= 1).
                if k > 0 {
                    let ps = spans[k - 1].1;
                    let pk = &spans[k - 1].0;
                    let pbody_len = spans[k].1 - ps;
                    let phead = &pk[..pk.len() - pbody_len];
                    let budget = if md {
                        max.saturating_sub(phead.chars().count()).max(1)
                    } else if k - 1 == 0 {
                        max
                    } else {
                        let hl = head_code.chars().count();
                        max.saturating_sub(hl).max(max / 2).max(1)
                    };
                    let mut len = 0usize;
                    let mut pos = ps;
                    let mut over = None;
                    for line in t[ps..].split_inclusive('\n') {
                        let n = line.chars().count();
                        if len > 0 && len + n > budget {
                            over = Some(pos);
                            break;
                        }
                        len += n;
                        pos += line.len();
                    }
                    let pos = over.expect("une coupe suppose un dépassement");
                    let w = budget * 15 / 100;
                    let want = safe
                        .iter()
                        .filter(|&&p| p > ps && p <= pos && t[p..pos].chars().count() <= w)
                        .min_by_key(|&&p| naive_weight(&t, p))
                        .copied()
                        .unwrap_or(pos);
                    assert_eq!(*start, want, "case {case} k {k} max {max}");
                }
                rebuilt.push_str(body);
            }
            assert_eq!(rebuilt, t);
        }
    }

    /// Texte Java : en-tête, remplissage sans position sûre, blocs (ligne vide puis deux
    /// lignes), puis une ligne qui dépasse. Rend le texte, la limite et les débuts des blocs.
    fn java_with_blocks(blocks: &[String]) -> (String, usize, Vec<usize>) {
        let mut t = String::from("public class A {\n    int pad;\n");
        for i in 0..50 {
            t.push_str(&format!("    int p{i:02};\n"));
        }
        let mut starts = Vec::new();
        for b in blocks {
            t.push('\n');
            starts.push(t.len());
            t.push_str(b);
        }
        let at = t.len();
        t.push_str("    int overflow;\n");
        let limit = limit_before(&t, at);
        (t, limit, starts)
    }

    fn block(tag: &str) -> String {
        format!("    int a{tag};\n    int b{tag};\n")
    }

    #[test]
    fn req288_ac7_plus_petite_empreinte_dans_la_fenetre() {
        // Trois positions sûres, la dernière n'a pas la plus petite empreinte.
        let w = |tag: &str| naive_weight(&block(tag), 0);
        let (a, b, c) = (0..200)
            .flat_map(|i| (0..200).map(move |j| (i, j)))
            .find_map(|(i, j)| {
                let (x, y, z) = (format!("x{i}"), format!("y{j}"), "z".to_string());
                (w(&y) < w(&x) && w(&y) < w(&z)).then_some((x, y, z))
            })
            .unwrap();
        let (t, limit, starts) = java_with_blocks(&[block(&a), block(&b), block(&c)]);
        let spans = split_spans(&t, limit);
        assert_eq!(spans[1].1, starts[1], "{spans:?}");
        assert_ne!(spans[1].1, starts[2]);
        // Deux positions de mêmes deux lignes, d'empreinte la plus petite : la première.
        let a = "k".to_string();
        let c = (0..500)
            .map(|j| format!("c{j}"))
            .find(|c| w(c) > w(&a))
            .unwrap();
        let (t, limit, starts) = java_with_blocks(&[block(&a), block(&c), block(&a)]);
        let spans = split_spans(&t, limit);
        assert_eq!(spans[1].1, starts[0], "{spans:?}");
    }

    #[test]
    fn req294_ac5_corps_suivants_stables_apres_insertion_en_tete() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let bodies = |spans: &[(String, usize)], t: &str| -> Vec<String> {
            (1..spans.len())
                .map(|k| {
                    let end = spans.get(k + 1).map_or(t.len(), |(_, s)| *s);
                    t[spans[k].1..end].to_string()
                })
                .collect()
        };
        let (mut same, total) = (0usize, 300usize);
        for case in 0..total {
            let md = case >= 200;
            let limit = 5000 + next(55_001) as usize;
            let target = limit * (3 + next(4) as usize) + limit / 2;
            let mut t = String::new();
            if md {
                t.push_str("# Guide\n");
                while t.len() < target {
                    if next(8) == 0 {
                        t.push_str(&format!("\n## S{}\n", next(1000)));
                    }
                    t.push_str(&"m".repeat(20 + next(60) as usize));
                    t.push('\n');
                }
            } else {
                t.push_str("class A {\n    int a;\n");
                while t.len() < target {
                    for _ in 0..(1 + next(5)) {
                        t.push_str("    ");
                        t.push_str(&"y".repeat(15 + next(60) as usize));
                        t.push_str(";\n");
                    }
                    t.push_str(if next(2) == 0 { "\n" } else { "    }\n" });
                }
            }
            let mut ins = String::new();
            for _ in 0..(1 + next(3)) {
                ins.push_str(&"i".repeat(1 + next(30) as usize));
                ins.push('\n');
            }
            let split = |s: &str| {
                if md {
                    split_spans_for("g.md", s, limit)
                } else {
                    split_spans(s, limit)
                }
            };
            let before = split(&t);
            let t2 = format!("{ins}{t}");
            let after = split(&t2);
            assert!(
                (4..=8).contains(&before.len()),
                "case {case}: {} morceaux",
                before.len()
            );
            if bodies(&before, &t) == bodies(&after, &t2) {
                same += 1;
            }
        }
        assert!(
            same * 100 >= total * 90,
            "taux de corps stables : {same}/{total}"
        );
    }
}
