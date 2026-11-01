//! Relevé et lignes de l'estimation du plan d'enrichissement (REQ-184, DEC-148) : fonctions pures,
//! sans accès au réseau ni au stockage. Le relevé est calculé par `enrich::estimate`.

use std::path::Path;

use crate::escape;

use super::enrich::MAX_CALLS_PER_FILE;

/// Jetons de sortie estimés par nœud demandé (DEC-058 : environ 40 jetons par résumé).
pub(crate) const OUTPUT_TOKENS_PER_NODE: usize = 40;

/// Jetons d'entrée estimés : un jeton pour quatre octets, arrondi par excès.
pub(crate) fn input_tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// Jetons de sortie estimés pour `nodes` nœuds demandés.
pub(crate) fn output_tokens(nodes: usize) -> usize {
    OUTPUT_TOKENS_PER_NODE * nodes
}

/// Cas d'un fichier envoyé seul.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AloneCase {
    /// Appels et octets comptés.
    Counted,
    /// Plan refusé au plafond d'appels : appels prévus, aucun compté.
    OverCap(usize),
    /// Fichier illisible : aucun appel compté.
    Unreadable,
}

/// Un fichier envoyé seul (unité `Big`) et ce qui en est compté.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AloneFile {
    pub(crate) path: String,
    pub(crate) calls: usize,
    pub(crate) bytes: usize,
    pub(crate) case: AloneCase,
}

/// Relevé de l'estimation, un essai par appel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Estimate {
    /// Fichiers à enrichir.
    pub(crate) files: usize,
    /// Fichiers envoyés seuls, au plafond et illisibles compris.
    pub(crate) sent_alone: usize,
    /// Fichiers sans nœud à demander (aucun appel).
    pub(crate) without_call: usize,
    pub(crate) calls: usize,
    pub(crate) bytes: usize,
    /// Nœuds demandés.
    pub(crate) nodes: usize,
    /// Fichiers au plafond d'appels : chemin et appels prévus.
    pub(crate) capped: Vec<(String, usize)>,
    /// Fichiers envoyés seuls et illisibles.
    pub(crate) unreadable: Vec<String>,
    /// Tous les fichiers envoyés seuls.
    pub(crate) alone: Vec<AloneFile>,
}

/// Lignes minimale ou maximale d'un même relevé : chaque nombre du relevé multiplié par `times`.
fn totals_line(e: &Estimate, qualifier: &str, times: usize, note: &str) -> String {
    format!(
        "estimation {qualifier} : {} appels, {} octets, {} jetons d'entrée, {} jetons de sortie ({note})",
        e.calls * times,
        e.bytes * times,
        input_tokens(e.bytes) * times,
        output_tokens(e.nodes) * times,
    )
}

/// Lignes de stdout de l'estimation (V-002), dans l'ordre : fichiers, minimale, maximale, fichiers
/// au plafond ou illisibles triés par chemin, puis `sans configuration` (chemin de la
/// configuration personnelle absente). `tries` = essais par appel de la ligne maximale
/// (`format_retries` + 1).
pub(crate) fn stdout_lines(e: &Estimate, tries: u32, no_config: Option<&Path>) -> Vec<String> {
    let mut lines = vec![
        format!(
            "estimation : {} fichiers à enrichir dont {} envoyés seuls et {} sans appel",
            e.files, e.sent_alone, e.without_call
        ),
        totals_line(e, "minimale", 1, "un essai par appel"),
        totals_line(
            e,
            "maximale",
            tries as usize,
            &format!("{tries} essais par appel"),
        ),
    ];
    let mut special: Vec<(&str, String)> = Vec::new();
    for a in &e.alone {
        let line = match a.case {
            AloneCase::Counted => continue,
            AloneCase::OverCap(planned) => format!(
                "estimation au plafond : {} ({planned} appels prévus, au plus {MAX_CALLS_PER_FILE} par fichier, aucun compté)",
                escape::path(&a.path)
            ),
            AloneCase::Unreadable => format!(
                "estimation illisible : {} (aucun appel compté)",
                escape::path(&a.path)
            ),
        };
        special.push((a.path.as_str(), line));
    }
    special.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    lines.extend(special.into_iter().map(|(_, l)| l));
    if let Some(p) = no_config {
        lines.push(format!(
            "estimation sans configuration : réglages par défaut ; nephila n'enrichit rien tant que {} n'existe pas",
            escape::path(&p.to_string_lossy())
        ));
    }
    lines
}

/// Lignes de trace `estimation <chemin> : <a> appels, <o> octets[ (<cas>)]` (V-005), une par
/// fichier envoyé seul, triées par chemin, chemin brut, sans le préfixe `trace:`.
pub(crate) fn trace_lines(e: &Estimate) -> Vec<String> {
    let mut alone: Vec<&AloneFile> = e.alone.iter().collect();
    alone.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    alone
        .into_iter()
        .map(|a| {
            let case = match a.case {
                AloneCase::Counted => String::new(),
                AloneCase::OverCap(p) => format!(" (au plafond, {p} appels prévus)"),
                AloneCase::Unreadable => " (illisible)".to_string(),
            };
            format!(
                "estimation {} : {} appels, {} octets{case}",
                a.path, a.calls, a.bytes
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alone(path: &str, calls: usize, bytes: usize, case: AloneCase) -> AloneFile {
        AloneFile {
            path: path.into(),
            calls,
            bytes,
            case,
        }
    }

    /// Relevé écrit en dur : 7 fichiers dont 2 seuls et 1 sans appel ; 5 appels, 98 412 octets
    /// (⌈98 412 / 4⌉ = 24 603 jetons), 37 nœuds (40 × 37 = 1 480 jetons).
    fn sample() -> Estimate {
        Estimate {
            files: 7,
            sent_alone: 3,
            without_call: 1,
            calls: 5,
            bytes: 98_412,
            nodes: 37,
            capped: vec![("src/gen/Big.java".into(), 134)],
            unreadable: vec!["a/Gone.md".into()],
            alone: vec![
                alone("src/gen/Big.java", 0, 0, AloneCase::OverCap(134)),
                alone("docs/guide.md", 3, 61_230, AloneCase::Counted),
                alone("a/Gone.md", 0, 0, AloneCase::Unreadable),
            ],
        }
    }

    #[test]
    fn token_formulas_round_up_and_scale() {
        assert_eq!(input_tokens(0), 0);
        assert_eq!(input_tokens(1), 1);
        assert_eq!(input_tokens(4), 1);
        assert_eq!(input_tokens(5), 2);
        assert_eq!(output_tokens(37), 1480);
    }

    #[test]
    fn stdout_lines_follow_v002_templates() {
        let lines = stdout_lines(&sample(), 3, None);
        assert_eq!(
            lines,
            vec![
                "estimation : 7 fichiers à enrichir dont 3 envoyés seuls et 1 sans appel",
                "estimation minimale : 5 appels, 98412 octets, 24603 jetons d'entrée, 1480 jetons de sortie (un essai par appel)",
                "estimation maximale : 15 appels, 295236 octets, 73809 jetons d'entrée, 4440 jetons de sortie (3 essais par appel)",
                "estimation illisible : a/Gone.md (aucun appel compté)",
                "estimation au plafond : src/gen/Big.java (134 appels prévus, au plus 50 par fichier, aucun compté)",
            ]
        );
    }

    #[test]
    fn maximum_equals_minimum_without_retries_and_figures_plural() {
        let e = Estimate {
            files: 1,
            sent_alone: 1,
            calls: 1,
            bytes: 5,
            nodes: 1,
            ..Estimate::default()
        };
        let lines = stdout_lines(&e, 1, None);
        assert_eq!(
            lines[0],
            "estimation : 1 fichiers à enrichir dont 1 envoyés seuls et 0 sans appel"
        );
        assert_eq!(
            lines[1],
            "estimation minimale : 1 appels, 5 octets, 2 jetons d'entrée, 40 jetons de sortie (un essai par appel)"
        );
        assert_eq!(
            lines[2],
            "estimation maximale : 1 appels, 5 octets, 2 jetons d'entrée, 40 jetons de sortie (1 essais par appel)"
        );
        // Le maximum multiplie les jetons d'entrée du minimum (pas ceux des octets multipliés).
        let l = stdout_lines(&e, 2, None);
        assert_eq!(
            l[2],
            "estimation maximale : 2 appels, 10 octets, 4 jetons d'entrée, 80 jetons de sortie (2 essais par appel)"
        );
    }

    #[test]
    fn up_to_date_graph_is_all_zeros() {
        let lines = stdout_lines(&Estimate::default(), 3, None);
        assert_eq!(
            lines,
            vec![
                "estimation : 0 fichiers à enrichir dont 0 envoyés seuls et 0 sans appel",
                "estimation minimale : 0 appels, 0 octets, 0 jetons d'entrée, 0 jetons de sortie (un essai par appel)",
                "estimation maximale : 0 appels, 0 octets, 0 jetons d'entrée, 0 jetons de sortie (3 essais par appel)",
            ]
        );
    }

    #[test]
    fn no_config_line_comes_last() {
        let lines = stdout_lines(
            &sample(),
            3,
            Some(Path::new("/home/ana/.config/nephila/config.toml")),
        );
        assert_eq!(lines.len(), 6);
        assert_eq!(
            lines[5],
            "estimation sans configuration : réglages par défaut ; nephila n'enrichit rien tant que /home/ana/.config/nephila/config.toml n'existe pas"
        );
    }

    #[test]
    fn control_characters_escaped_on_stdout_raw_in_trace() {
        let e = Estimate {
            alone: vec![alone("a/\u{1b}Big.md", 0, 0, AloneCase::OverCap(51))],
            capped: vec![("a/\u{1b}Big.md".into(), 51)],
            ..Estimate::default()
        };
        let lines = stdout_lines(&e, 3, None);
        assert_eq!(
            lines[3],
            "estimation au plafond : a/\\x1BBig.md (51 appels prévus, au plus 50 par fichier, aucun compté)"
        );
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')));
        assert_eq!(
            trace_lines(&e),
            vec!["estimation a/\u{1b}Big.md : 0 appels, 0 octets (au plafond, 51 appels prévus)"]
        );
    }

    #[test]
    fn trace_lines_sorted_by_path_with_cases() {
        assert_eq!(
            trace_lines(&sample()),
            vec![
                "estimation a/Gone.md : 0 appels, 0 octets (illisible)",
                "estimation docs/guide.md : 3 appels, 61230 octets",
                "estimation src/gen/Big.java : 0 appels, 0 octets (au plafond, 134 appels prévus)",
            ]
        );
    }
}
