//! Stockage SQLite du graphe dans `<dépôt>/.rkg/graph.db` (DEC-009, DEC-010).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

use crate::error::{Error, Result};
use crate::model::{Edge, EdgeKind, FileRecord, FileState, Node, NodeKind, Ref, SubKind};

/// Version du schéma, stockée dans `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 2;

const SCHEMA: &str = include_str!("schema.sql");

fn db_err(e: rusqlite::Error) -> Error {
    Error::Message(format!("base du graphe : {e}"))
}

/// Chemin du fichier de base pour un dépôt.
pub fn db_path(repo: &Path) -> PathBuf {
    repo.join(".rkg").join("graph.db")
}

pub struct Store {
    conn: Connection,
    read_only: bool,
}

impl Store {
    /// Ouvre le graphe en écriture, en le créant (dossier `.rkg/`, schéma, WAL) si besoin.
    pub fn open_or_create(repo: &Path) -> Result<Store> {
        let path = db_path(repo);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                Error::Message(format!("impossible de créer {} : {e}", dir.display()))
            })?;
        }
        let conn = Connection::open(&path).map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        let store = Store {
            conn,
            read_only: false,
        };
        match store.user_version()? {
            0 => store.create_schema()?,
            v if v == SCHEMA_VERSION => {}
            // Schéma plus ancien : build repart de zéro, on le recrée.
            v if v > 0 && v < SCHEMA_VERSION => store.recreate_schema()?,
            v => return Err(version_error(v)),
        }
        Ok(store)
    }

    /// Ouvre le graphe en lecture seule, sans rien créer.
    pub fn open_readonly(repo: &Path) -> Result<Store> {
        let path = db_path(repo);
        if !path.is_file() {
            return Err(Error::Message(format!(
                "aucun graphe dans {} : lancez `rkg build`",
                repo.display()
            )));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        let store = Store {
            conn,
            read_only: true,
        };
        match store.user_version()? {
            0 => {
                return Err(Error::Message(format!(
                    "graphe vide ou illisible dans {} : lancez `rkg build`",
                    path.display()
                )))
            }
            v if v == SCHEMA_VERSION => {}
            v => return Err(version_error(v)),
        }
        Ok(store)
    }

    fn user_version(&self) -> Result<i32> {
        self.conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(db_err)
    }

    /// Supprime toutes les tables de l'ancien schéma puis crée le schéma courant.
    fn recreate_schema(&self) -> Result<()> {
        let names: Vec<String> = {
            let mut st = self
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
                .map_err(db_err)?;
            let rows = st.query_map([], |r| r.get(0)).map_err(db_err)?;
            rows.collect::<std::result::Result<_, _>>()
                .map_err(db_err)?
        };
        let mut sql = String::from("PRAGMA foreign_keys = OFF;\nBEGIN;\n");
        for n in names {
            sql.push_str(&format!(
                "DROP TABLE IF EXISTS \"{}\";\n",
                n.replace('"', "\"\"")
            ));
        }
        sql.push_str("COMMIT;\nPRAGMA foreign_keys = ON;");
        self.conn.execute_batch(&sql).map_err(db_err)?;
        self.create_schema()
    }

    fn create_schema(&self) -> Result<()> {
        // Schéma et version dans une seule transaction : jamais de base à moitié créée.
        let sql = format!(
            "BEGIN;\n{SCHEMA}\nINSERT INTO meta(key, value) VALUES ('schema_version', '{SCHEMA_VERSION}');\nPRAGMA user_version = {SCHEMA_VERSION};\nCOMMIT;"
        );
        self.conn.execute_batch(&sql).map_err(db_err)
    }

    fn ensure_writable(&self) -> Result<()> {
        if self.read_only {
            Err(Error::Message("graphe ouvert en lecture seule".to_string()))
        } else {
            Ok(())
        }
    }

    /// Vide entièrement le graphe (fichiers, nœuds, références, liens, résumé, meta sauf version).
    pub fn reset(&mut self) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute_batch(
            "DELETE FROM edges; DELETE FROM refs; DELETE FROM declarations; DELETE FROM nodes; DELETE FROM files;
             DELETE FROM summary; DELETE FROM meta WHERE key <> 'schema_version';",
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Vide le graphe et enregistre `files` « en attente » en une seule transaction : le graphe
    /// précédent n'est remplacé que si l'ensemble réussit.
    pub fn reset_with_pending_files(&mut self, files: &[FileRecord]) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute_batch(
            "DELETE FROM edges; DELETE FROM refs; DELETE FROM declarations; DELETE FROM nodes; DELETE FROM files;
             DELETE FROM summary; DELETE FROM meta WHERE key <> 'schema_version';",
        )
        .map_err(db_err)?;
        {
            let mut st = tx
                .prepare("INSERT INTO files(path, hash, size, mtime, state) VALUES (?1, ?2, ?3, ?4, 'pending')")
                .map_err(db_err)?;
            for f in files {
                let size = i64::try_from(f.size)
                    .map_err(|_| Error::Message(format!("taille invalide pour {}", f.path)))?;
                st.execute(params![f.path, f.hash, size, f.mtime])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Enregistre ou met à jour des fichiers (état d'origine conservé dans chaque enregistrement),
    /// en une seule transaction. Un fichier déjà connu garde ses nœuds.
    pub fn insert_files(&mut self, files: &[FileRecord]) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        {
            let mut st = tx
                .prepare(
                    "INSERT INTO files(path, hash, size, mtime, state) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(path) DO UPDATE SET hash = excluded.hash, size = excluded.size,
                         mtime = excluded.mtime, state = excluded.state",
                )
                .map_err(db_err)?;
            for f in files {
                let size = i64::try_from(f.size)
                    .map_err(|_| Error::Message(format!("taille invalide pour {}", f.path)))?;
                st.execute(params![f.path, f.hash, size, f.mtime, f.state.as_str()])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Insère des fichiers à l'état « en attente ».
    pub fn insert_pending_files(&mut self, files: &[FileRecord]) -> Result<()> {
        let pending: Vec<FileRecord> = files
            .iter()
            .cloned()
            .map(|mut f| {
                f.state = FileState::Pending;
                f
            })
            .collect();
        self.insert_files(&pending)
    }

    /// Remplace en une transaction les déclarations et références d'un fichier, puis le marque
    /// indexé. Le fichier doit être connu. Un nœud peut être déclaré par plusieurs fichiers
    /// (DEC-014) : la ligne du nœud (attributs, fichier de rattachement) est celle du premier
    /// déclarant, les références restent attachées au fichier qui les a émises. Un nœud sans
    /// fichier (dépendance, concept) est déclaré lui aussi par le fichier qui le mentionne et
    /// disparaît avec son dernier déclarant. Les références dont la source n'est pas déclarée
    /// par le fichier sont rejetées.
    pub fn replace_file(&mut self, path: &str, nodes: &[Node], refs: &[Ref]) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        let known: bool = tx
            .query_row("SELECT 1 FROM files WHERE path = ?1", [path], |_| Ok(true))
            .unwrap_or(false);
        if !known {
            return Err(Error::Message(format!(
                "fichier inconnu du graphe : {path}"
            )));
        }
        release_file(&tx, path)?;
        {
            let mut ins = tx
                .prepare(
                    "INSERT OR IGNORE INTO nodes(id, kind, qualified, short, file, subkind, summary)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(db_err)?;
            let mut decl = tx
                .prepare("INSERT OR IGNORE INTO declarations(file, node) VALUES (?1, ?2)")
                .map_err(db_err)?;
            for n in nodes {
                if n.file.as_deref().is_some_and(|f| f != path) {
                    return Err(Error::Message(format!(
                        "le nœud {} n'appartient pas au fichier {path}",
                        n.id
                    )));
                }
                ins.execute(params![
                    n.id,
                    n.kind.as_str(),
                    n.qualified,
                    n.short,
                    path_or_null(n, path),
                    n.subkind.map(SubKind::as_str),
                    n.summary
                ])
                .map_err(db_err)?;
                decl.execute(params![path, n.id]).map_err(db_err)?;
            }
            let mut insr = tx
                .prepare(
                    "INSERT INTO refs(source, file, kind, target_name, context)
                     SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS
                       (SELECT 1 FROM declarations WHERE node = ?1 AND file = ?2)",
                )
                .map_err(db_err)?;
            for r in refs {
                let n = insr
                    .execute(params![
                        r.source,
                        path,
                        r.kind.as_str(),
                        r.target_name,
                        r.context
                    ])
                    .map_err(db_err)?;
                if n == 0 {
                    return Err(Error::Message(format!(
                        "référence dont la source {} n'est pas déclarée dans {path}",
                        r.source
                    )));
                }
            }
        }
        tx.execute("UPDATE files SET state = 'indexed' WHERE path = ?1", [path])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Supprime un fichier, ses nœuds, références et les liens attachés. Renvoie `false` si inconnu.
    pub fn remove_file(&mut self, path: &str) -> Result<bool> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        release_file(&tx, path)?;
        let n = tx
            .execute("DELETE FROM files WHERE path = ?1", [path])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(n > 0)
    }

    /// Remplace l'ensemble des liens dérivés (résolution, REQ-008).
    pub fn replace_edges(&mut self, edges: &[Edge]) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM edges", []).map_err(db_err)?;
        {
            let mut st = tx
                .prepare("INSERT OR IGNORE INTO edges(source, target, kind) VALUES (?1, ?2, ?3)")
                .map_err(db_err)?;
            for e in edges {
                st.execute(params![e.source, e.target, e.kind.as_str()])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    pub fn nodes(&self) -> Result<Vec<Node>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes ORDER BY id",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, qualified, short, file, subkind, summary) = row.map_err(db_err)?;
            out.push(Node {
                id,
                kind: NodeKind::parse(&kind)?,
                qualified,
                short,
                file,
                subkind: subkind.as_deref().map(SubKind::parse).transpose()?,
                summary,
            });
        }
        Ok(out)
    }

    pub fn edges(&self) -> Result<Vec<Edge>> {
        let mut st = self
            .conn
            .prepare("SELECT source, target, kind FROM edges ORDER BY source, target, kind")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (source, target, kind) = row.map_err(db_err)?;
            out.push(Edge {
                source,
                target,
                kind: EdgeKind::parse(&kind)?,
            });
        }
        Ok(out)
    }

    pub fn refs(&self) -> Result<Vec<Ref>> {
        let mut st = self
            .conn
            .prepare("SELECT source, kind, target_name, context FROM refs ORDER BY id")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (source, kind, target_name, context) = row.map_err(db_err)?;
            out.push(Ref {
                source,
                kind: EdgeKind::parse(&kind)?,
                target_name,
                context,
            });
        }
        Ok(out)
    }

    /// Couples (fichier, identifiant de nœud) des déclarations.
    pub fn declarations(&self) -> Result<Vec<(String, String)>> {
        let mut st = self
            .conn
            .prepare("SELECT file, node FROM declarations ORDER BY file, node")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_err)?;
        rows.map(|r| r.map_err(db_err)).collect()
    }

    pub fn files(&self) -> Result<Vec<FileRecord>> {
        let mut st = self
            .conn
            .prepare("SELECT path, hash, size, mtime, state FROM files ORDER BY path")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (path, hash, size, mtime, state) = row.map_err(db_err)?;
            out.push(FileRecord {
                path,
                hash,
                size: size.max(0) as u64,
                mtime,
                state: match state.as_str() {
                    "indexed" => FileState::Indexed,
                    _ => FileState::Pending,
                },
            });
        }
        Ok(out)
    }

    /// Valeur du mode de journalisation (« wal » attendu).
    pub fn journal_mode(&self) -> Result<String> {
        self.conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .map_err(db_err)
    }

    pub fn schema_version(&self) -> Result<i32> {
        self.user_version()
    }

    /// Résumé global du dépôt et son horodatage (millisecondes Unix), s'il existe.
    pub fn summary(&self) -> Result<Option<(String, i64)>> {
        match self.conn.query_row(
            "SELECT text, computed_at FROM summary WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    pub fn set_summary(&mut self, text: &str, computed_at: i64) -> Result<()> {
        self.ensure_writable()?;
        self.conn
            .execute(
                "INSERT INTO summary(id, text, computed_at) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET text = excluded.text, computed_at = excluded.computed_at",
                params![text, computed_at],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

/// Retire les déclarations et références d'un fichier, rattache ses nœuds partagés à un autre
/// déclarant, et supprime les nœuds devenus orphelins (liens attachés compris).
fn release_file(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<()> {
    tx.execute("DELETE FROM refs WHERE file = ?1", [path])
        .map_err(db_err)?;
    tx.execute("DELETE FROM declarations WHERE file = ?1", [path])
        .map_err(db_err)?;
    tx.execute(
        "UPDATE nodes SET file = (SELECT MIN(file) FROM declarations WHERE node = nodes.id)
         WHERE file = ?1 AND EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
        [path],
    )
    .map_err(db_err)?;
    tx.execute(
        "DELETE FROM nodes WHERE NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
        [],
    )
    .map_err(db_err)?;
    Ok(())
}

fn path_or_null<'a>(n: &Node, path: &'a str) -> Option<&'a str> {
    n.file.as_ref().map(|_| path)
}

fn version_error(v: i32) -> Error {
    Error::Message(format!(
        "version de schéma du graphe non prise en charge ({v}, attendue {SCHEMA_VERSION}) : relancez `rkg build`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{node_id, parse_node_id};
    use tempfile::tempdir;

    fn file(path: &str) -> FileRecord {
        FileRecord {
            path: path.into(),
            hash: "h".into(),
            size: 3,
            mtime: 7,
            state: FileState::Indexed,
        }
    }

    fn ty(q: &str, f: &str) -> Node {
        Node::new(
            NodeKind::Type,
            q,
            q.rsplit('.').next().unwrap(),
            Some(f),
            Some(SubKind::Class),
            None,
        )
    }

    #[test]
    fn req005_ac1_node_kinds() {
        let names: Vec<_> = NodeKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "module",
                "dependency",
                "package",
                "type",
                "method",
                "document",
                "section",
                "concept"
            ]
        );
        for k in NodeKind::ALL {
            assert_eq!(NodeKind::parse(k.as_str()).unwrap(), *k);
        }
        assert!(NodeKind::parse("bogus").is_err());
        assert_eq!(
            node_id(NodeKind::Type, "com.acme.OrderService"),
            "type:com.acme.OrderService"
        );
        let (k, q) = parse_node_id("method:com.acme.OrderService.place").unwrap();
        assert_eq!((k, q), (NodeKind::Method, "com.acme.OrderService.place"));
        assert!(parse_node_id("sansgenre").is_err());
    }

    #[test]
    fn req005_ac2_edge_kinds_and_rules() {
        use EdgeKind as E;
        use NodeKind as N;
        assert_eq!(EdgeKind::ALL.len(), 8);
        assert!(E::DependsOn.allows(N::Module, N::Dependency));
        assert!(E::DependsOn.allows(N::Module, N::Module));
        assert!(!E::DependsOn.allows(N::Type, N::Module));
        assert!(E::Imports.allows(N::Type, N::Type));
        assert!(E::Extends.allows(N::Type, N::Type));
        assert!(E::Implements.allows(N::Type, N::Type));
        assert!(E::Contains.allows(N::Type, N::Method));
        assert!(E::Contains.allows(N::Package, N::Type));
        assert!(E::Contains.allows(N::Module, N::Package));
        assert!(E::Contains.allows(N::Document, N::Section));
        assert!(!E::Contains.allows(N::Method, N::Type));
        assert!(E::LinksTo.allows(N::Document, N::Document));
        assert!(E::Mentions.allows(N::Section, N::Type));
        assert!(E::Mentions.allows(N::Section, N::Concept));
        assert!(!E::Mentions.allows(N::Section, N::Section));
        assert!(!E::Mentions.allows(N::Document, N::Type));
        assert!(E::Calls.allows(N::Method, N::Method));
        assert!(!E::Calls.allows(N::Type, N::Method));
    }

    #[test]
    fn req005_ac4_subkinds() {
        for s in [
            "class",
            "interface",
            "enum",
            "record",
            "annotation",
            "object",
            "trait",
        ] {
            assert_eq!(SubKind::parse(s).unwrap().as_str(), s);
        }
        assert!(SubKind::parse("struct").is_err());
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("A.scala")]).unwrap();
        let mut n = ty("a.T", "A.scala");
        n.subkind = Some(SubKind::Trait);
        st.replace_file("A.scala", std::slice::from_ref(&n), &[])
            .unwrap();
        assert_eq!(st.nodes().unwrap(), vec![n]);
    }

    #[test]
    fn req009_ac2_db_under_rkg_only_wal_and_version() {
        let dir = tempdir().unwrap();
        let st = Store::open_or_create(dir.path()).unwrap();
        assert!(dir.path().join(".rkg/graph.db").is_file());
        assert_eq!(st.journal_mode().unwrap(), "wal");
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
        drop(st);
        let top: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(top, [".rkg"]);
        // Réouverture d'une base existante.
        Store::open_or_create(dir.path()).unwrap();
    }

    #[test]
    fn req009_ac4_readonly_missing_creates_nothing() {
        let dir = tempdir().unwrap();
        let err = Store::open_readonly(dir.path()).err().unwrap();
        assert!(err.to_string().contains("rkg build"), "{err}");
        assert!(!dir.path().join(".rkg").exists());
    }

    #[test]
    fn readonly_reads_and_refuses_writes() {
        let dir = tempdir().unwrap();
        let mut w = Store::open_or_create(dir.path()).unwrap();
        w.insert_pending_files(&[file("a.md")]).unwrap();
        let mut r = Store::open_readonly(dir.path()).unwrap();
        assert_eq!(r.files().unwrap().len(), 1);
        assert!(r.reset().is_err());
        assert!(r.insert_pending_files(&[file("b.md")]).is_err());
    }

    #[test]
    fn older_schema_version_is_recreated_for_build() {
        let dir = tempdir().unwrap();
        drop(Store::open_or_create(dir.path()).unwrap());
        let c = Connection::open(db_path(dir.path())).unwrap();
        c.execute_batch("DROP TABLE summary; CREATE TABLE summary(x); PRAGMA user_version = 1;")
            .unwrap();
        drop(c);
        assert!(Store::open_readonly(dir.path()).is_err());
        let st = Store::open_or_create(dir.path()).unwrap();
        drop(st);
        assert!(Store::open_readonly(dir.path()).is_ok());
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let dir = tempdir().unwrap();
        drop(Store::open_or_create(dir.path()).unwrap());
        let c = Connection::open(db_path(dir.path())).unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();
        drop(c);
        assert!(Store::open_or_create(dir.path()).is_err());
        assert!(Store::open_readonly(dir.path()).is_err());
    }

    #[test]
    fn pending_files_then_replace_marks_indexed() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.java"), file("b.java")])
            .unwrap();
        assert!(st
            .files()
            .unwrap()
            .iter()
            .all(|f| f.state == FileState::Pending));
        let n = ty("p.A", "a.java");
        let r = Ref {
            source: n.id.clone(),
            kind: EdgeKind::Extends,
            target_name: "Base".into(),
            context: Some("import p.Base;".into()),
        };
        st.replace_file("a.java", std::slice::from_ref(&n), std::slice::from_ref(&r))
            .unwrap();
        let files = st.files().unwrap();
        assert_eq!(files[0].state, FileState::Indexed);
        assert_eq!(files[1].state, FileState::Pending);
        assert_eq!(st.refs().unwrap(), vec![r]);
        // Remplacement : l'ancien contenu disparaît.
        let n2 = ty("p.A2", "a.java");
        st.replace_file("a.java", std::slice::from_ref(&n2), &[])
            .unwrap();
        assert_eq!(st.nodes().unwrap(), vec![n2]);
        assert!(st.refs().unwrap().is_empty());
    }

    #[test]
    fn replace_file_is_atomic_on_error() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.java")]).unwrap();
        let n = ty("p.A", "a.java");
        st.replace_file("a.java", std::slice::from_ref(&n), &[])
            .unwrap();
        let bad = Ref {
            source: "type:inconnu".into(),
            kind: EdgeKind::Extends,
            target_name: "X".into(),
            context: None,
        };
        assert!(st
            .replace_file("a.java", &[ty("p.B", "a.java")], &[bad])
            .is_err());
        assert_eq!(st.nodes().unwrap(), vec![n]);
        assert!(st.replace_file("zzz", &[], &[]).is_err());
        // Nœud rattaché à un autre fichier : rejeté.
        assert!(st
            .replace_file("a.java", &[ty("p.C", "autre.java")], &[])
            .is_err());
    }

    #[test]
    fn external_nodes_have_no_file_and_duplicates_are_merged() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("pom.xml")]).unwrap();
        let dep = Node::new(
            NodeKind::Dependency,
            "org.slf4j:slf4j-api",
            "slf4j-api",
            None,
            None,
            None,
        );
        let m = Node::new(
            NodeKind::Module,
            "core",
            "core",
            Some("pom.xml"),
            None,
            Some("résumé"),
        );
        st.replace_file("pom.xml", &[m.clone(), dep.clone(), m.clone()], &[])
            .unwrap();
        let nodes = st.nodes().unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&dep) && nodes.contains(&m));
    }

    #[test]
    fn remove_file_deletes_nodes_refs_and_edges() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.java"), file("b.java")])
            .unwrap();
        let a = ty("p.A", "a.java");
        let b = ty("p.B", "b.java");
        st.replace_file(
            "a.java",
            std::slice::from_ref(&a),
            &[Ref {
                source: a.id.clone(),
                kind: EdgeKind::Imports,
                target_name: "B".into(),
                context: None,
            }],
        )
        .unwrap();
        st.replace_file("b.java", std::slice::from_ref(&b), &[])
            .unwrap();
        st.replace_edges(&[Edge {
            source: a.id.clone(),
            target: b.id.clone(),
            kind: EdgeKind::Imports,
        }])
        .unwrap();
        assert_eq!(st.edges().unwrap().len(), 1);
        assert!(st.remove_file("b.java").unwrap());
        assert!(!st.remove_file("b.java").unwrap());
        assert_eq!(st.nodes().unwrap(), vec![a]);
        assert!(st.edges().unwrap().is_empty());
        assert_eq!(st.refs().unwrap().len(), 1);
        assert!(st.remove_file("a.java").unwrap());
        assert!(st.refs().unwrap().is_empty());
    }

    #[test]
    fn reset_empties_graph_and_summary_roundtrip() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        st.set_summary("texte", 42).unwrap();
        assert_eq!(st.summary().unwrap(), Some(("texte".to_string(), 42)));
        st.set_summary("autre", 43).unwrap();
        assert_eq!(st.summary().unwrap(), Some(("autre".to_string(), 43)));
        st.reset().unwrap();
        assert!(st.files().unwrap().is_empty());
        assert!(st.nodes().unwrap().is_empty());
        assert_eq!(st.summary().unwrap(), None);
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn node_declared_by_two_files_keeps_refs_of_both() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.java"), file("b.java")])
            .unwrap();
        let mk = |f: &str, t: &str| {
            (
                ty("p.A", f),
                Ref {
                    source: "type:p.A".into(),
                    kind: EdgeKind::Imports,
                    target_name: t.into(),
                    context: None,
                },
            )
        };
        let (na, ra) = mk("a.java", "X");
        let (nb, rb) = mk("b.java", "Y");
        st.replace_file("a.java", &[na], &[ra]).unwrap();
        st.replace_file("b.java", &[nb], &[rb]).unwrap();
        assert_eq!(st.nodes().unwrap().len(), 1);
        assert_eq!(st.refs().unwrap().len(), 2);
        assert!(st
            .files()
            .unwrap()
            .iter()
            .all(|f| f.state == FileState::Indexed));
        // Le premier déclarant part : le nœud reste, rattaché au second.
        assert!(st.remove_file("a.java").unwrap());
        let nodes = st.nodes().unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].file.as_deref(), Some("b.java"));
        assert_eq!(st.refs().unwrap().len(), 1);
        // Le dernier part : le nœud disparaît.
        assert!(st.remove_file("b.java").unwrap());
        assert!(st.nodes().unwrap().is_empty());
    }

    #[test]
    fn fileless_nodes_follow_their_declaring_files() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("pom.xml"), file("b/pom.xml")])
            .unwrap();
        let dep = Node::new(
            NodeKind::Dependency,
            "org.slf4j:slf4j-api",
            "slf4j-api",
            None,
            None,
            None,
        );
        st.replace_file("pom.xml", std::slice::from_ref(&dep), &[])
            .unwrap();
        st.replace_file("b/pom.xml", std::slice::from_ref(&dep), &[])
            .unwrap();
        assert!(st.remove_file("pom.xml").unwrap());
        assert_eq!(st.nodes().unwrap(), vec![dep.clone()]);
        // Mise à jour du fichier déclarant : la dépendance disparaît si elle n'est plus déclarée.
        st.replace_file("b/pom.xml", &[], &[]).unwrap();
        assert!(st.nodes().unwrap().is_empty());
        st.replace_file("b/pom.xml", std::slice::from_ref(&dep), &[])
            .unwrap();
        assert!(st.remove_file("b/pom.xml").unwrap());
        assert!(st.nodes().unwrap().is_empty());
    }
}
