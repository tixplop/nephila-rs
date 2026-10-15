//! Extraction d'un document Markdown (CommonMark) : sections, liens et mentions candidates
//! (REQ-023, DEC-017, DEC-021).

use std::collections::HashSet;
use std::path::Path;

use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};

use super::FileStructure;
use crate::model::{node_id, EdgeKind, NodeKind};
use crate::parse::{Declared, Reference};

/// Ancre « à la GitHub » : minuscules, espaces → `-`, ponctuation retirée sauf `-` et `_`.
fn anchor_of(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars() {
        if c.is_whitespace() {
            out.push('-');
        } else if c == '-' || c == '_' || c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Chemin du document au format `a/b/c.md` (séparateurs `/`).
fn doc_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_markdown_path(p: &str) -> bool {
    let lower = p.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// Cible d'un lien, normalisée depuis le dossier de `doc`, si c'est un document Markdown relatif
/// resté dans le dépôt.
fn link_target(doc: &str, dest: &str) -> Option<String> {
    let dest = dest.trim();
    let end = dest.find(['#', '?']).unwrap_or(dest.len());
    let rel = &dest[..end];
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') {
        return None;
    }
    // Un schéma (`https:`, `mailto:`…) précède tout `/`.
    if let Some(i) = rel.find(':') {
        if !rel[..i].contains('/') {
            return None;
        }
    }
    if !is_markdown_path(rel) {
        return None;
    }
    let mut parts: Vec<&str> = match doc.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').collect(),
        None => Vec::new(),
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    let out = parts.join("/");
    is_markdown_path(&out).then_some(out)
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Nom JVM d'un span de code (`A`, `a.b.C`, `C.m()`, `C#m`) → cible (sans `()`, `#` → `.`).
fn mention_target(code: &str) -> Option<String> {
    let s = code.trim();
    let s = s.strip_suffix("()").unwrap_or(s);
    if s.is_empty() || s.matches('#').count() > 1 {
        return None;
    }
    let normalized = s.replace('#', ".");
    normalized.split('.').all(is_ident).then_some(normalized)
}

struct State {
    doc: String,
    doc_id: String,
    out: FileStructure,
    anchors: HashSet<String>,
    current: Option<String>,
    seen: HashSet<(String, EdgeKind, String)>,
}

impl State {
    fn add_ref(&mut self, source: String, kind: EdgeKind, target: String) {
        if self.seen.insert((source.clone(), kind, target.clone())) {
            self.out.references.push(Reference {
                source,
                kind,
                target,
            });
        }
    }

    fn add_mention(&mut self, code: &str) {
        let (Some(section), Some(target)) = (self.current.clone(), mention_target(code)) else {
            return;
        };
        self.add_ref(section, EdgeKind::Mentions, target);
    }

    fn open_section(&mut self, title: &str) {
        let base = anchor_of(title);
        let mut anchor = base.clone();
        let mut n = 0;
        while !self.anchors.insert(anchor.clone()) {
            n += 1;
            anchor = format!("{base}-{n}");
        }
        let qualified = format!("{}#{anchor}", self.doc);
        self.out.declares.push(Declared {
            kind: NodeKind::Section,
            qualified: qualified.clone(),
            subkind: None,
            summary: None,
            short: None,
        });
        self.current = Some(node_id(NodeKind::Section, &qualified));
    }
}

/// Extrait le document, ses sections, ses liens vers d'autres documents et ses mentions candidates.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let doc = doc_path(path);
    let mut st = State {
        doc_id: node_id(NodeKind::Document, &doc),
        doc,
        out: FileStructure::default(),
        anchors: HashSet::new(),
        current: None,
        seen: HashSet::new(),
    };
    st.out.declares.push(Declared {
        kind: NodeKind::Document,
        qualified: st.doc.clone(),
        subkind: None,
        summary: None,
        short: None,
    });
    // Titre en cours de lecture : texte et spans de code rencontrés.
    let mut heading: Option<(String, Vec<String>)> = None;
    for ev in Parser::new(text) {
        match ev {
            Event::Start(Tag::Heading { .. }) => heading = Some((String::new(), Vec::new())),
            Event::End(TagEnd::Heading(_)) => {
                if let Some((title, codes)) = heading.take() {
                    st.open_section(&title);
                    for c in codes {
                        st.add_mention(&c);
                    }
                }
            }
            Event::Start(Tag::Link {
                dest_url,
                link_type,
                ..
            }) if link_type != LinkType::Email => {
                if let Some(t) = link_target(&st.doc, &dest_url) {
                    let src = st.doc_id.clone();
                    st.add_ref(src, EdgeKind::LinksTo, t);
                }
            }
            Event::Text(t) => {
                if let Some((title, _)) = heading.as_mut() {
                    title.push_str(&t);
                }
            }
            Event::Code(c) => match heading.as_mut() {
                Some((title, codes)) => {
                    title.push_str(&c);
                    codes.push(c.to_string());
                }
                None => st.add_mention(&c),
            },
            Event::SoftBreak | Event::HardBreak => {
                if let Some((title, _)) = heading.as_mut() {
                    title.push(' ');
                }
            }
            _ => {}
        }
    }
    st.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, text: &str) -> FileStructure {
        extract(Path::new(path), text)
    }

    fn ids(f: &FileStructure) -> Vec<String> {
        f.declares.iter().map(Declared::id).collect()
    }

    fn refs(f: &FileStructure, kind: EdgeKind) -> Vec<(String, String)> {
        f.references
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| (r.source.clone(), r.target.clone()))
            .collect()
    }

    #[test]
    fn req023_ac1_document_et_sections_atx() {
        let f = run("docs/archi.md", "# Architecture\n\ntexte\n\n## Détails\n");
        assert_eq!(
            ids(&f),
            [
                "document:docs/archi.md",
                "section:docs/archi.md#architecture",
                "section:docs/archi.md#détails"
            ]
        );
    }

    #[test]
    fn req023_ac1_sections_setext_et_ponctuation() {
        let f = run(
            "a.md",
            "Titre Un!\n=========\n\nMon_titre, v2.0 - ok\n---\n",
        );
        assert_eq!(
            ids(&f),
            [
                "document:a.md",
                "section:a.md#titre-un",
                "section:a.md#mon_titre-v20---ok"
            ]
        );
    }

    #[test]
    fn req023_ac2_liens_inline_reference_et_filtrage() {
        let text = "# T\n[guide](guide.md) [accueil](../README.md#install) [site](https://x.org/a.md)\n\
                    [ref][r] [autre](notes.txt) [abs](/x.md) [out](../../../x.md) [m](mailto:a@b.md)\n\n\
                    [r]: sub/./other.markdown\n";
        let f = run("docs/archi.md", text);
        let src = "document:docs/archi.md".to_string();
        assert_eq!(
            refs(&f, EdgeKind::LinksTo),
            [
                (src.clone(), "docs/guide.md".to_string()),
                (src.clone(), "README.md".to_string()),
                (src, "docs/sub/other.markdown".to_string()),
            ]
        );
    }

    #[test]
    fn req023_ac2_autolien_relatif_et_absent() {
        let f = run("a.md", "<b.md> et [x](absent.md)");
        let t: Vec<String> = refs(&f, EdgeKind::LinksTo)
            .into_iter()
            .map(|r| r.1)
            .collect();
        // `<b.md>` n'est pas un autolien CommonMark : seul le lien inline compte.
        assert_eq!(t, ["absent.md"]);
    }

    #[test]
    fn req023_ac2_autolien_email_ignore() {
        let f = run("a.md", "<a@b.md> et <contact@acme.md>");
        assert!(refs(&f, EdgeKind::LinksTo).is_empty());
    }

    #[test]
    fn req023_ac3_mentions() {
        let text = "# Architecture\n\nVoir `OrderService`, `OrderRepository.save()` et `OrderRepository#save`, \
                    `a.b.C`, `C#m()`.\n";
        let f = run("docs/archi.md", text);
        let s = "section:docs/archi.md#architecture".to_string();
        let t: Vec<String> = refs(&f, EdgeKind::Mentions)
            .into_iter()
            .map(|(src, t)| {
                assert_eq!(src, s);
                t
            })
            .collect();
        assert_eq!(t, ["OrderService", "OrderRepository.save", "a.b.C", "C.m"]);
    }

    #[test]
    fn req023_ac3_pas_un_nom_jvm() {
        let f = run(
            "a.md",
            "# T\n`a b` `f(x)` `` `x-y` `1a` `a#b#c` `a..b` `-`\n",
        );
        assert!(refs(&f, EdgeKind::Mentions).is_empty());
    }

    #[test]
    fn req023_ac4_pas_de_mention_hors_span_bloc_ou_avant_titre() {
        let text = "`Avant`\n\n# T\n\nOrderService nu\n\n```java\nOrderService x; `Dans`\n```\n\n    OrderService\n";
        let f = run("a.md", text);
        assert!(refs(&f, EdgeKind::Mentions).is_empty());
    }

    #[test]
    fn req023_mention_dans_titre_et_section_suivante() {
        let f = run("a.md", "# Le `Foo` central\n\n## B\n`Bar`\n");
        assert_eq!(
            refs(&f, EdgeKind::Mentions),
            [
                ("section:a.md#le-foo-central".to_string(), "Foo".to_string()),
                ("section:a.md#b".to_string(), "Bar".to_string())
            ]
        );
    }

    #[test]
    fn req023_ac5_titres_en_double() {
        let f = run("d.md", "## Exemple\n\n## Exemple\n\n## Exemple\n");
        assert_eq!(
            ids(&f),
            [
                "document:d.md",
                "section:d.md#exemple",
                "section:d.md#exemple-1",
                "section:d.md#exemple-2"
            ]
        );
    }

    #[test]
    fn req023_document_vide() {
        let f = run("x.md", "");
        assert_eq!(ids(&f), ["document:x.md"]);
        assert!(f.references.is_empty());
    }
}
