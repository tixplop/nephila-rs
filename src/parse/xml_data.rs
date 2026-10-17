//! Fichiers XML hors build : document et candidats de nom JVM des valeurs d'attribut et des
//! textes d'élément (REQ-031, DEC-038).

use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::data::{self, Candidates, Format};
use super::FileStructure;

fn flush(buf: &mut String, source: &str, cands: &mut Candidates) {
    if !buf.is_empty() {
        cands.add_value(source, buf);
        buf.clear();
    }
}

fn attributes(e: &BytesStart<'_>, source: &str, cands: &mut Candidates) {
    for a in e.attributes().flatten() {
        let value = match a.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
            Ok(v) => v.into_owned(),
            // Entité non déclarée : texte laissé tel quel.
            Err(_) => a.value.to_string(),
        };
        cands.add_value(source, &value);
    }
}

/// Extrait le document et les mentions d'un fichier XML de données.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let source = data::document_id(path);
    let mut cands = Candidates::new();
    let mut reader = Reader::from_str(text);
    let mut depth = 0usize;
    let mut open: Option<String> = None;
    let mut problem = None;
    // Texte contigu (texte, CDATA, références d'entité) : une valeur.
    let mut buf = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(t)) => buf.push_str(&t.xml10_content()),
            Ok(Event::CData(t)) => buf.push_str(&t.xml10_content()),
            Ok(Event::GeneralRef(r)) => match r.resolve_char_ref() {
                Ok(Some(c)) => buf.push(c),
                _ => match r.xml10_content().as_ref() {
                    "lt" => buf.push('<'),
                    "gt" => buf.push('>'),
                    "amp" => buf.push('&'),
                    "quot" => buf.push('"'),
                    "apos" => buf.push('\''),
                    other => {
                        buf.push('&');
                        buf.push_str(other);
                        buf.push(';');
                    }
                },
            },
            Ok(Event::Start(e)) => {
                flush(&mut buf, &source, &mut cands);
                depth += 1;
                open = Some(e.name().as_ref().to_string());
                attributes(&e, &source, &mut cands);
            }
            Ok(Event::Empty(e)) => {
                flush(&mut buf, &source, &mut cands);
                attributes(&e, &source, &mut cands);
            }
            Ok(Event::End(_)) => {
                flush(&mut buf, &source, &mut cands);
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => {
                flush(&mut buf, &source, &mut cands);
                if depth > 0 {
                    problem = Some(format!(
                        "document tronqué (élément <{}> non fermé)",
                        open.unwrap_or_default()
                    ));
                }
                break;
            }
            Ok(_) => flush(&mut buf, &source, &mut cands),
            Err(e) => {
                flush(&mut buf, &source, &mut cands);
                problem = Some(format!("{e} (position {})", reader.error_position()));
                break;
            }
        }
    }
    let (references, cap) = cands.finish(path);
    let mut warnings = Vec::new();
    if let Some(p) = problem {
        warnings.push(data::warning(path, Format::Xml, &p));
    }
    warnings.extend(cap);
    FileStructure {
        context: None,
        declares: vec![data::document(path, Format::Xml)],
        references,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &str, text: &str) -> FileStructure {
        extract(Path::new(p), text)
    }

    fn targets(f: &FileStructure) -> Vec<String> {
        f.references.iter().map(|r| r.target.clone()).collect()
    }

    /// AC-031-1
    #[test]
    fn req031_ac1_beans() {
        let t = r#"<beans><bean id="orderService" class="com.acme.core.OrderService"><property name="repo" ref="orderRepository"/></bean><bean class="x"><constructor-arg><value>com.acme.core.OrderRepository</value></constructor-arg></bean></beans>"#;
        let f = run("src/main/resources/beans.xml", t);
        assert_eq!(f.declares[0].id(), "document:src/main/resources/beans.xml");
        assert_eq!(
            targets(&f),
            [
                "com.acme.core.OrderService",
                "com.acme.core.OrderRepository"
            ]
        );
        assert!(f
            .references
            .iter()
            .all(|r| r.source == "document:src/main/resources/beans.xml"));
        assert!(f.warnings.is_empty());
    }

    /// AC-031-2
    #[test]
    fn req031_ac2_cibles_normalisees() {
        let t = "<a><v>com.acme.core.OrderRepository#save</v><v><![CDATA[com.acme.core.OrderService]]></v><v>com.acme.core</v><v>com.acme.Absent</v></a>";
        let f = run("a.xml", t);
        assert_eq!(
            targets(&f),
            [
                "com.acme.core.OrderRepository.save",
                "com.acme.core.OrderService",
                "com.acme.core",
                "com.acme.Absent"
            ]
        );
    }

    /// AC-031-4
    #[test]
    fn req031_ac4_tronque() {
        let f = run(
            "WEB-INF/web.xml",
            "<web-app><servlet><servlet-class>com.acme.core.OrderService</servlet-class>",
        );
        assert_eq!(targets(&f), ["com.acme.core.OrderService"]);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("WEB-INF/web.xml"));
    }

    #[test]
    fn ignore_noms_prologue_doctype_commentaires_et_instructions() {
        let t = "<?xml version=\"1.0\"?><!DOCTYPE a.b.C><!-- x.y.Z --><?pi p.q.R?><a.b.c d.e.f=\"1\"><x:y.z/></a.b.c>";
        let f = run("a.xml", t);
        assert!(f.references.is_empty(), "{:?}", f.references);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    }

    #[test]
    fn entites_declarees_et_non_declarees() {
        let f = run("a.xml", "<a b=\"p.Q&#32;r.S\"><v>&undeclared; p.T</v></a>");
        assert_eq!(targets(&f), ["p.Q", "r.S", "p.T"]);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        let f = run("b.xml", "<a b=\"p.Q &bogus; p.R\"/>");
        assert_eq!(targets(&f), ["p.Q", "p.R"]);
    }

    #[test]
    fn fichier_vide_document_seul_sans_avertissement() {
        let f = run("e.xml", "");
        assert_eq!(f.declares.len(), 1);
        assert!(f.warnings.is_empty() && f.references.is_empty());
    }

    /// AC-029-3
    #[test]
    fn req029_ac3_plafond_de_candidats() {
        let body: String = (0..1_500).map(|i| format!("<v>p.C{i}</v>")).collect();
        let f = run("big.xml", &format!("<a>{body}</a>"));
        assert_eq!(f.references.len(), 1_000);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("big.xml") && f.warnings[0].contains("500"));
    }

    /// AC-029-5 (partie `t.xml`)
    #[test]
    fn ac029_5_t_xml() {
        let f = run("t.xml", "<a><b>");
        assert_eq!(f.declares.len(), 1);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("t.xml"));
    }

    #[test]
    fn erreur_de_lecture_garde_les_candidats_lus() {
        let f = run("a.xml", "<a><v>p.Q</v></b>");
        assert_eq!(targets(&f), ["p.Q"]);
        assert_eq!(f.warnings.len(), 1);
    }
}
