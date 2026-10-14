//! Modèle du graphe : genres de nœuds, de liens et sous-genres (REQ-005).

use crate::error::{Error, Result};

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($var),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),+ }
            }

            pub fn parse(s: &str) -> Result<$name> {
                match s {
                    $($s => Ok($name::$var),)+
                    other => Err(Error::Message(format!(
                        "{} inconnu : « {other} »", stringify!($name)
                    ))),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum! {
    /// Genre d'un nœud (AC-005-1).
    NodeKind {
        Module => "module",
        Dependency => "dependency",
        Package => "package",
        Type => "type",
        Method => "method",
        Document => "document",
        Section => "section",
        Concept => "concept",
    }
}

string_enum! {
    /// Genre d'un lien (AC-005-2).
    EdgeKind {
        DependsOn => "depends_on",
        Imports => "imports",
        Extends => "extends",
        Implements => "implements",
        Contains => "contains",
        LinksTo => "links_to",
        Mentions => "mentions",
        Calls => "calls",
    }
}

string_enum! {
    /// Sous-genre d'un nœud de genre `type` (AC-005-4).
    SubKind {
        Class => "class",
        Interface => "interface",
        Enum => "enum",
        Record => "record",
        Annotation => "annotation",
        Object => "object",
        Trait => "trait",
    }
}

impl NodeKind {
    /// Nœud de code (cible possible d'un `mentions`).
    pub fn is_code(self) -> bool {
        matches!(
            self,
            NodeKind::Module
                | NodeKind::Dependency
                | NodeKind::Package
                | NodeKind::Type
                | NodeKind::Method
        )
    }
}

impl EdgeKind {
    /// Indique si un lien de ce genre est permis entre ces deux genres de nœuds.
    pub fn allows(self, from: NodeKind, to: NodeKind) -> bool {
        use NodeKind as N;
        match self {
            EdgeKind::DependsOn => from == N::Module && matches!(to, N::Module | N::Dependency),
            EdgeKind::Imports | EdgeKind::Extends | EdgeKind::Implements => {
                from == N::Type && to == N::Type
            }
            EdgeKind::Contains => matches!(
                (from, to),
                (N::Type, N::Method)
                    | (N::Package, N::Type)
                    | (N::Module, N::Package)
                    | (N::Document, N::Section)
            ),
            EdgeKind::LinksTo => from == N::Document && to == N::Document,
            EdgeKind::Mentions => from == N::Section && (to.is_code() || to == N::Concept),
            EdgeKind::Calls => from == N::Method && to == N::Method,
        }
    }
}

/// Identifiant de nœud : `genre:nom.qualifié`.
pub fn node_id(kind: NodeKind, qualified: &str) -> String {
    format!("{}:{}", kind.as_str(), qualified)
}

/// Découpe un identifiant en genre et nom qualifié.
pub fn parse_node_id(id: &str) -> Result<(NodeKind, &str)> {
    let (k, q) = id
        .split_once(':')
        .ok_or_else(|| Error::Message(format!("identifiant de nœud invalide : « {id} »")))?;
    Ok((NodeKind::parse(k)?, q))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub qualified: String,
    pub short: String,
    /// Fichier source ; `None` pour une dépendance externe ou un concept.
    pub file: Option<String>,
    pub subkind: Option<SubKind>,
    pub summary: Option<String>,
}

impl Node {
    pub fn new(
        kind: NodeKind,
        qualified: &str,
        short: &str,
        file: Option<&str>,
        subkind: Option<SubKind>,
        summary: Option<&str>,
    ) -> Node {
        Node {
            id: node_id(kind, qualified),
            kind,
            qualified: qualified.to_string(),
            short: short.to_string(),
            file: file.map(str::to_string),
            subkind,
            summary: summary.map(str::to_string),
        }
    }
}

/// Référence par nom, non résolue (DEC-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub source: String,
    pub kind: EdgeKind,
    /// Nom cible tel qu'écrit dans le fichier.
    pub target_name: String,
    /// Contexte du fichier (package, imports), texte libre.
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Indexed,
    Pending,
}

impl FileState {
    pub fn as_str(self) -> &'static str {
        match self {
            FileState::Indexed => "indexed",
            FileState::Pending => "pending",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
    /// Instant (ms depuis l'epoch) de l'enregistrement, posé par le stockage à l'écriture ;
    /// la valeur fournie par l'appelant est ignorée.
    pub recorded_at: i64,
    pub state: FileState,
}
