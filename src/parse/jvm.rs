//! Socle commun aux extracteurs JVM (Java, Kotlin, Scala, Groovy) : noms qualifiés, pile des
//! types englobants, contexte de résolution et références d'imports (REQ-006, DEC-016).

use std::collections::HashSet;

use crate::model::{node_id, EdgeKind, NodeKind, SubKind};
use crate::parse::{Declared, OrderedSet, Reference};

use super::FileStructure;

/// Nom de la méthode qui représente constructeurs et blocs d'initialisation.
pub const INIT: &str = "<init>";

/// Import d'un fichier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Import {
    pub(super) path: String,
    pub(super) wildcard: bool,
    pub(super) is_static: bool,
}

/// Construit la structure d'un fichier au fil du parcours de l'arbre.
#[derive(Debug, Default)]
pub struct Builder {
    package: Option<String>,
    imports: OrderedSet<Import>,
    declares: Vec<Declared>,
    references: OrderedSet<Reference>,
    seen: HashSet<String>,
    /// Noms qualifiés des types englobants, du plus externe au plus interne.
    stack: Vec<String>,
    /// Identifiants des types de premier niveau.
    top_level: Vec<String>,
}

/// Retire arguments génériques, espaces et accolades de fin d'un nom de type écrit dans le code.
pub fn clean_type_name(raw: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();
    for c in raw.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

/// Nom de fichier sans extension, réduit à des caractères d'identifiant (`_` sinon), pour les
/// types de fichier (`NomFichier`, `NomFichier$package`).
pub fn file_stem_ident(path: &std::path::Path) -> Option<String> {
    let stem: String = path
        .file_stem()?
        .to_str()?
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    (!stem.is_empty()).then_some(stem)
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Déclare le package du fichier (le dernier déclaré l'emporte pour le contexte).
    pub fn set_package(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.package = Some(name.to_string());
        self.push_declared(NodeKind::Package, name.to_string(), None);
    }

    /// Rétablit le package courant à la sortie d'un `package p { … }` (sans rien déclarer).
    pub fn restore_package(&mut self, package: Option<String>) {
        self.package = package;
    }

    pub fn package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    /// Enregistre un import. `path` est le nom importé sans le `.*` final.
    pub fn add_import(&mut self, path: &str, wildcard: bool, is_static: bool) {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        let imp = Import {
            path: path.to_string(),
            wildcard,
            is_static,
        };
        self.imports.insert(imp);
    }

    /// Nom qualifié d'un type de nom simple `name` déclaré à l'emplacement courant.
    pub fn qualify_type(&self, name: &str) -> String {
        match (self.stack.last(), &self.package) {
            (Some(outer), _) => format!("{outer}.{name}"),
            (None, Some(p)) => format!("{p}.{name}"),
            (None, None) => name.to_string(),
        }
    }

    /// Entre dans un type : le déclare et le pose sur la pile. Renvoie son nom qualifié.
    pub fn enter_type(&mut self, name: &str, subkind: SubKind) -> String {
        let q = self.qualify_type(name);
        if self.stack.is_empty() {
            self.top_level.push(node_id(NodeKind::Type, &q));
        }
        self.push_declared(NodeKind::Type, q.clone(), Some(subkind));
        self.stack.push(q.clone());
        q
    }

    pub fn exit_type(&mut self) {
        self.stack.pop();
    }

    /// Pile des types englobants (noms qualifiés), du plus externe au plus interne.
    pub fn type_stack(&self) -> &[String] {
        &self.stack
    }

    /// Type englobant courant (nom qualifié).
    pub fn current_type(&self) -> Option<&str> {
        self.stack.last().map(String::as_str)
    }

    /// Déclare (une seule fois : surcharges fusionnées) la méthode `name` du type courant.
    /// Renvoie son nom qualifié, ou `None` hors de tout type.
    pub fn declare_method(&mut self, name: &str) -> Option<String> {
        let q = format!("{}.{}", self.stack.last()?, name);
        self.push_declared(NodeKind::Method, q.clone(), None);
        Some(q)
    }

    /// Déclare un type (ex. type de fichier `NomFichierKt`) sans le placer sur la pile.
    pub fn declare_file_type(&mut self, name: &str, subkind: SubKind) -> String {
        let q = self.qualify_type(name);
        if self.stack.is_empty() {
            self.top_level.push(node_id(NodeKind::Type, &q));
        }
        self.push_declared(NodeKind::Type, q.clone(), Some(subkind));
        q
    }

    /// Ajoute une référence par nom depuis le nœud `source` (identifiant `genre:nom`).
    pub fn add_ref(&mut self, source_id: &str, kind: EdgeKind, target: &str) {
        let target = clean_type_name(target);
        if target.is_empty() {
            return;
        }
        let r = Reference {
            source: source_id.to_string(),
            kind,
            target,
        };
        self.references.insert(r);
    }

    /// Référence depuis un type (nom qualifié).
    pub fn add_type_ref(&mut self, source_qualified: &str, kind: EdgeKind, target: &str) {
        self.add_ref(&node_id(NodeKind::Type, source_qualified), kind, target);
    }

    fn push_declared(&mut self, kind: NodeKind, qualified: String, subkind: Option<SubKind>) {
        if self.seen.insert(node_id(kind, &qualified)) {
            self.declares.push(Declared {
                kind,
                qualified,
                subkind,
                summary: None,
                short: None,
            });
        }
    }

    /// Contexte au format de `resolve::parse_context` : `package …` puis `import …`.
    fn context(&self) -> Option<String> {
        let mut lines = Vec::new();
        if let Some(p) = &self.package {
            lines.push(format!("package {p}"));
        }
        for i in self.imports.as_slice() {
            lines.push(format!(
                "import {}{}{}",
                if i.is_static { "static " } else { "" },
                i.path,
                if i.wildcard { ".*" } else { "" }
            ));
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Termine : ajoute les imports (chaque type de premier niveau importe chaque type importé
    /// explicitement ; jokers et imports statiques ne donnent pas de lien).
    pub fn finish(mut self) -> FileStructure {
        let explicit: Vec<String> = self
            .imports
            .as_slice()
            .iter()
            .filter(|i| !i.wildcard && !i.is_static)
            .map(|i| i.path.clone())
            .collect();
        let tops = std::mem::take(&mut self.top_level);
        for src in &tops {
            for target in &explicit {
                self.add_ref(src, EdgeKind::Imports, target);
            }
        }
        FileStructure {
            context: self.context(),
            declares: self.declares,
            references: self.references.into_vec(),
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_qualifies_et_imbriques() {
        let mut b = Builder::new();
        b.set_package("a.b");
        let o = b.enter_type("Outer", SubKind::Class);
        assert_eq!(o, "a.b.Outer");
        let i = b.enter_type("Inner", SubKind::Class);
        assert_eq!(i, "a.b.Outer.Inner");
        assert_eq!(b.declare_method("m").unwrap(), "a.b.Outer.Inner.m");
        b.exit_type();
        assert_eq!(b.current_type(), Some("a.b.Outer"));
    }

    #[test]
    fn declare_file_type_n_empile_pas() {
        let mut b = Builder::new();
        b.set_package("a");
        assert_eq!(b.declare_file_type("FooKt", SubKind::Class), "a.FooKt");
        assert_eq!(b.current_type(), None);
        assert_eq!(b.declare_method("f"), None);
        assert_eq!(b.enter_type("T", SubKind::Class), "a.T");
    }

    #[test]
    fn surcharges_fusionnees_et_contexte() {
        let mut b = Builder::new();
        b.add_import("x.Y", false, false);
        b.add_import("x.util", true, false);
        b.add_import("x.S.f", false, true);
        b.enter_type("T", SubKind::Class);
        b.declare_method("m");
        b.declare_method("m");
        let s = b.finish();
        assert_eq!(s.declares.len(), 2);
        assert_eq!(
            s.context.as_deref(),
            Some("import x.Y\nimport x.util.*\nimport static x.S.f")
        );
        assert_eq!(s.references.len(), 1);
        assert_eq!(s.references[0].target, "x.Y");
    }

    #[test]
    fn generiques_retires() {
        assert_eq!(clean_type_name("List<Map<A, B>> "), "List");
        assert_eq!(clean_type_name("a.B<C>.D"), "a.B.D");
    }
}
