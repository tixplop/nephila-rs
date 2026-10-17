//! Fichiers de données (SQL, XML, JSON, YAML) : document, candidats de nom JVM et avertissement
//! commun (REQ-029, DEC-038).

use std::collections::HashSet;
use std::path::Path;

use super::{Declared, Reference};
use crate::model::{node_id, EdgeKind, NodeKind, SubKind};

/// Plafond de candidats de nom JVM par fichier (REQ-029).
pub const MAX_CANDIDATES: usize = 1_000;

/// Format d'un fichier de données.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Sql,
    Xml,
    Json,
    Yaml,
}

impl Format {
    pub fn subkind(self) -> SubKind {
        match self {
            Format::Sql => SubKind::Sql,
            Format::Xml => SubKind::Xml,
            Format::Json => SubKind::Json,
            Format::Yaml => SubKind::Yaml,
        }
    }

    /// Nom du format dans les avertissements.
    pub fn label(self) -> &'static str {
        match self {
            Format::Sql => "SQL",
            Format::Xml => "XML",
            Format::Json => "JSON",
            Format::Yaml => "YAML",
        }
    }
}

/// Chemin au format `a/b/c.ext` (séparateurs `/`).
pub fn doc_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Nœud `document:<chemin>` d'un fichier de données, de sous-genre = format.
pub fn document(path: &Path, format: Format) -> Declared {
    Declared {
        kind: NodeKind::Document,
        qualified: doc_path(path),
        subkind: Some(format.subkind()),
        summary: None,
        short: None,
    }
}

/// Identifiant du nœud document d'un fichier de données.
pub fn document_id(path: &Path) -> String {
    node_id(NodeKind::Document, &doc_path(path))
}

/// Avertissement d'un fichier mal formé, lu au mieux.
pub fn warning(path: &Path, format: Format, problem: &str) -> String {
    format!(
        "{} : {} mal formé, lu au mieux : {problem}",
        doc_path(path),
        format.label()
    )
}

fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Nom cible d'un morceau s'il est un candidat de nom JVM.
fn candidate(piece: &str) -> Option<String> {
    let body = if let Some(b) = piece.strip_suffix("()") {
        b.to_string()
    } else if let Some((b, seg)) = piece.split_once('#') {
        if !is_segment(seg) {
            return None;
        }
        format!("{b}.{seg}")
    } else {
        piece.to_string()
    };
    let name = body.replace('$', ".");
    let mut count = 0;
    for seg in name.split('.') {
        if !is_segment(seg) {
            return None;
        }
        count += 1;
    }
    (count >= 2).then_some(name)
}

/// Candidats de nom JVM d'une valeur : découpage sur les blancs, `,` et `;` (REQ-029), dans
/// l'ordre de la valeur.
pub fn jvm_candidates(value: &str) -> Vec<String> {
    value
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|p| !p.is_empty())
        .filter_map(candidate)
        .collect()
}

/// Références `mentions` candidates d'un fichier : dédoublonnées par (source, nom), au plus
/// `MAX_CANDIDATES` dans l'ordre d'ajout.
#[derive(Debug, Default)]
pub struct Candidates {
    seen: HashSet<(String, String)>,
    refs: Vec<Reference>,
    ignored: usize,
}

impl Candidates {
    pub fn new() -> Candidates {
        Candidates::default()
    }

    /// Ajoute les candidats de nom JVM de `value`, rattachés à `source` (identifiant de nœud).
    pub fn add_value(&mut self, source: &str, value: &str) {
        for name in jvm_candidates(value) {
            self.add_name(source, name);
        }
    }

    /// Ajoute un nom cible déjà normalisé.
    pub fn add_name(&mut self, source: &str, name: String) {
        if !self.seen.insert((source.to_string(), name.clone())) {
            return;
        }
        if self.refs.len() >= MAX_CANDIDATES {
            self.ignored += 1;
            return;
        }
        self.refs.push(Reference {
            source: source.to_string(),
            kind: EdgeKind::Mentions,
            target: name,
        });
    }

    /// Références retenues et, au-delà du plafond, l'avertissement nommant le fichier.
    pub fn finish(self, path: &Path) -> (Vec<Reference>, Option<String>) {
        let warning = (self.ignored > 0).then(|| {
            format!(
                "{} : {} candidats de nom JVM ignorés (plafond de 1 000)",
                doc_path(path),
                self.ignored
            )
        });
        (self.refs, warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(v: &str) -> Vec<String> {
        jvm_candidates(v)
    }

    /// AC-029-2
    #[test]
    fn req029_ac2_exemples_de_candidats() {
        assert_eq!(c("com.acme.OrderService"), ["com.acme.OrderService"]);
        assert_eq!(c("com.acme.Outer$Inner"), ["com.acme.Outer.Inner"]);
        assert_eq!(c("Outer$Inner"), ["Outer.Inner"]);
        assert_eq!(c("com.acme.Repo#save"), ["com.acme.Repo.save"]);
        assert_eq!(c("com.acme.Repo.save()"), ["com.acme.Repo.save"]);
        assert_eq!(c("a._b1.C_2"), ["a._b1.C_2"]);
    }

    /// AC-029-2
    #[test]
    fn req029_ac2_non_candidats() {
        for v in [
            "orderService",
            "1.0",
            "v1.2",
            "${db.url}",
            "http://x.org/a.b",
            "classpath:com.acme",
            "a..b",
            ".a.b",
            "a.b.",
            "a.1b",
            "a.b#",
            "a.b#1",
            "a.b#c#d",
            "a.b()()",
            "",
            "   ",
            "é.a",
        ] {
            assert!(c(v).is_empty(), "{v:?} -> {:?}", c(v));
        }
    }

    /// AC-029-2
    #[test]
    fn req029_ac2_decoupage_sur_blancs_virgules_et_points_virgules() {
        assert_eq!(
            c("com.acme.A, com.acme.B;com.acme.C"),
            ["com.acme.A", "com.acme.B", "com.acme.C"]
        );
        assert_eq!(c("  x.y\tz.w\n"), ["x.y", "z.w"]);
        assert_eq!(c("localhost, com.acme.A true"), ["com.acme.A"]);
    }

    #[test]
    fn candidates_dedoublonne_par_source_et_nom() {
        let mut k = Candidates::new();
        k.add_value("document:a.xml", "x.A x.A x.B");
        k.add_value("key:a.xml#k", "x.A");
        let (refs, w) = k.finish(Path::new("a.xml"));
        assert!(w.is_none());
        let got: Vec<(&str, &str)> = refs
            .iter()
            .map(|r| (r.source.as_str(), r.target.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("document:a.xml", "x.A"),
                ("document:a.xml", "x.B"),
                ("key:a.xml#k", "x.A")
            ]
        );
        assert!(refs.iter().all(|r| r.kind == EdgeKind::Mentions));
    }

    #[test]
    fn candidates_plafonne_a_1000_dans_l_ordre_d_ajout() {
        let mut k = Candidates::new();
        for i in 0..1_500 {
            k.add_value("document:a.xml", &format!("p.C{i}"));
        }
        // Un doublon d'un candidat retenu ou ignoré ne change pas le compte.
        k.add_value("document:a.xml", "p.C0 p.C1400");
        let (refs, w) = k.finish(Path::new("db/a.xml"));
        assert_eq!(refs.len(), 1_000);
        assert_eq!(refs[0].target, "p.C0");
        assert_eq!(refs[999].target, "p.C999");
        assert_eq!(
            w.unwrap(),
            "db/a.xml : 500 candidats de nom JVM ignorés (plafond de 1 000)"
        );
    }

    #[test]
    fn candidates_sous_le_plafond_sans_avertissement() {
        let mut k = Candidates::new();
        for i in 0..1_000 {
            k.add_value("document:a.xml", &format!("p.C{i}"));
        }
        let (refs, w) = k.finish(Path::new("a.xml"));
        assert_eq!(refs.len(), 1_000);
        assert!(w.is_none());
    }

    #[test]
    fn document_et_avertissement() {
        let d = document(Path::new("db/V1.sql"), Format::Sql);
        assert_eq!(d.id(), "document:db/V1.sql");
        assert_eq!(d.subkind, Some(SubKind::Sql));
        assert_eq!(document_id(Path::new("db/V1.sql")), "document:db/V1.sql");
        assert_eq!(
            warning(Path::new("a/t.json"), Format::Json, "EOF"),
            "a/t.json : JSON mal formé, lu au mieux : EOF"
        );
    }
}
