//! Compaction du texte envoyé au LLM : licence de tête et blancs superflus (REQ-272, DEC-201).
//! Fonction pure : jamais d'erreur, le fichier sur disque n'est pas touché.

use crate::filter::FileType;
use crate::llm::enrich::{prompt_family, PromptFamily};
use crate::parse::markdown::code_block_ranges;

/// Texte compacté d'un fichier : un chemin sans type et le YAML partent tels quels.
pub fn compact(path: &str, text: &str) -> String {
    let Some(ft) = FileType::of(path) else {
        return text.to_string();
    };
    if ft == FileType::Yaml {
        return text.to_string();
    }
    let text = if prompt_family(ft) == PromptFamily::Code {
        strip_license(text)
    } else {
        text
    };
    let ranges = if ft == FileType::Markdown {
        code_block_ranges(text)
    } else {
        Vec::new()
    };
    squeeze(text, &ranges)
}

/// Retire le commentaire `/* … */` qui ouvre le fichier s'il parle de licence, avec les blancs
/// (BOM compris) qui l'entourent.
fn strip_license(text: &str) -> &str {
    let blank = |c: char| c.is_whitespace() || c == '\u{feff}';
    let head = text.trim_start_matches(blank);
    if !head.starts_with("/*") {
        return text;
    }
    let Some(end) = head[2..].find("*/").map(|i| i + 4) else {
        return text;
    };
    let lower = head[..end].to_lowercase();
    if ["copyright", "license", "spdx"]
        .iter()
        .any(|w| lower.contains(w))
    {
        head[end..].trim_start_matches(blank)
    } else {
        text
    }
}

/// Retire les blancs de fin de ligne et réduit les suites de lignes vides à une seule, sauf sur
/// les lignes qui croisent une plage protégée (triée).
fn squeeze(text: &str, protected: &[std::ops::Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_blank = false;
    let mut first = 0;
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        while first < protected.len() && protected[first].end <= start {
            first += 1;
        }
        let guarded = protected
            .get(first)
            .is_some_and(|r| r.start < end && start < r.end);
        if guarded {
            out.push_str(line);
            prev_blank = false;
        } else {
            let (body, eol) = if let Some(b) = line.strip_suffix("\r\n") {
                (b, "\r\n")
            } else if let Some(b) = line.strip_suffix('\n') {
                (b, "\n")
            } else {
                (line, "")
            };
            let body = body.trim_end_matches([' ', '\t']);
            if body.is_empty() {
                if !prev_blank {
                    out.push_str(eol);
                }
                prev_blank = true;
            } else {
                out.push_str(body);
                out.push_str(eol);
                prev_blank = false;
            }
        }
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::markdown::section_offsets;

    const CODE: [&str; 6] = ["a.java", "a.kt", "a.groovy", "a.scala", "a.js", "a.ts"];
    const OTHER: [&str; 9] = [
        "build.gradle.kts",
        "pom.xml",
        "ivy.xml",
        "package.json",
        "a.sql",
        "a.xml",
        "a.json",
        "a.md",
        "a.yml",
    ];
    const NON_YAML: [&str; 14] = [
        "a.java",
        "a.kt",
        "a.groovy",
        "a.scala",
        "a.js",
        "a.ts",
        "build.gradle.kts",
        "pom.xml",
        "ivy.xml",
        "package.json",
        "a.sql",
        "a.xml",
        "a.json",
        "a.md",
    ];

    #[test]
    fn req272_ac1_license_comment_is_removed() {
        for p in CODE {
            assert_eq!(
                compact(p, "/*\n * Copyright 2020 ACME\n */\n\npackage a;\n"),
                "package a;\n",
                "{p}"
            );
            assert_eq!(
                compact(p, "/**\n * Copyright\n */\npackage a;\n"),
                "package a;\n"
            );
            for w in ["LICENSE", "Licensed to X", "SPDX-License-Identifier: MIT"] {
                assert_eq!(compact(p, &format!("/* {w} */\n\nx\n")), "x\n", "{p} {w}");
            }
            assert_eq!(
                compact(p, "\u{feff}\n\n  /* Copyright */\n\u{feff}\n\nx\n"),
                "x\n"
            );
        }
    }

    #[test]
    fn req272_ac2_other_leading_text_is_kept() {
        for (p, t) in [
            ("a.java", "/* Service */\nclass A {}\n"),
            ("a.java", "package a;\n/* Copyright */\n"),
            ("a.java", "// Copyright 2020\nclass A {}\n"),
            ("a.js", "#!/x\n/* Copyright */\n"),
            ("a.java", "/* Copyright\nclass A {}\n"),
        ] {
            assert_eq!(compact(p, t), t, "{t}");
        }
        assert_eq!(
            compact("a.java", "/* Copyright A */\n/* Copyright B */\nx\n"),
            "/* Copyright B */\nx\n"
        );
    }

    #[test]
    fn req272_ac3_license_is_kept_outside_code_family() {
        for p in OTHER {
            assert_eq!(
                compact(p, "/* Copyright */\nx\n"),
                "/* Copyright */\nx\n",
                "{p}"
            );
        }
    }

    #[test]
    fn req272_ac4_blanks_are_squeezed() {
        for p in NON_YAML {
            assert_eq!(compact(p, "a  \t\nb\n"), "a\nb\n", "{p}");
            assert_eq!(compact(p, "a\n\n\n \t\n\nb\n"), "a\n\nb\n", "{p}");
            assert_eq!(compact(p, "a\n\nb\n"), "a\n\nb\n", "{p}");
            assert_eq!(compact(p, "a  \r\n\r\n\r\nb\r\n"), "a\r\n\r\nb\r\n", "{p}");
            assert_eq!(compact(p, "a  "), "a", "{p}");
        }
    }

    #[test]
    fn req272_ac5_yaml_and_unknown_are_untouched() {
        let t = "a: 1  \n\n\n\nb:  |\n  x  \n";
        for p in ["a.yml", "a.yaml", "a.txt", "Makefile"] {
            assert_eq!(compact(p, t), t, "{p}");
        }
    }

    #[test]
    fn req272_ac6_markdown_code_blocks_are_kept() {
        let blocks = [
            "```\nx  \n\n\n\ny  \n```\n",
            "~~~\nx  \n\n\n\ny  \n~~~\n",
            "    x  \n\n\n\n    y  \n",
            "- item\n\n  ```\n  x  \n\n\n\n  y  \n  ```\n",
            "```\nx  \n\n\n\ny  \n",
        ];
        for b in blocks {
            let text = format!("# T  \n\n\n\navant  \n\n{b}\n\n\n## U\napres  \n");
            let got = compact("a.md", &text);
            assert!(got.contains(b.trim_end_matches('\n')), "{b:?} -> {got:?}");
            assert!(got.starts_with("# T\n\navant\n\n"), "{got:?}");
            let titles = |t: &str| {
                section_offsets(t)
                    .into_iter()
                    .map(|(a, _)| a)
                    .collect::<Vec<_>>()
            };
            assert_eq!(titles(&text), titles(&got), "{b:?}");
        }
        let quote = "> ```\n> x  \n>\n>\n> y  \n> ```\n";
        assert!(compact("a.md", quote).contains("> x  \n>\n>\n> y  \n"));
        assert_eq!(compact("a.md", "a  \nb\n"), "a\nb\n");
    }

    #[test]
    fn req272_ac7_compact_is_idempotent_and_total() {
        let samples = [
            "/* Copyright */\n\n\nx  \n\n\n\ny\t\n",
            "é à  \n\n\n\nü\n",
            "",
            "\n\n\n",
            "/* Copyright */",
            "  ",
            "a\r\n\r\n\r\n",
            "```\nx  \n\n\n```\n\n\n\n",
        ];
        for p in CODE.iter().chain(OTHER.iter()).chain(["a.txt"].iter()) {
            for s in samples {
                let once = compact(p, s);
                assert_eq!(compact(p, &once), once, "{p} {s:?}");
            }
            assert_eq!(compact(p, "a\n\nb\n"), "a\n\nb\n");
        }
    }
}
