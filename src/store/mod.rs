//! Stockage SQLite du graphe dans `<dépôt>/.nephila/graph.db` (DEC-009, DEC-010).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

use crate::error::{Error, Result};
use crate::model::{
    Edge, EdgeKind, FileRecord, FileState, Node, NodeKind, Ref, RefOrigin, SubKind,
};

/// Instant courant en millisecondes depuis l'epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Version du schéma, stockée dans `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 4;

const SCHEMA: &str = include_str!("schema.sql");

fn db_err(e: rusqlite::Error) -> Error {
    Error::Message(format!("base du graphe : {e}"))
}

/// Chemin du fichier de base pour un dépôt.
pub fn db_path(repo: &Path) -> PathBuf {
    repo.join(".nephila").join("graph.db")
}

pub struct Store {
    conn: Connection,
    read_only: bool,
}

impl Store {
    /// Ouvre le graphe en écriture, en le créant (dossier `.nephila/`, schéma, WAL) si besoin.
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
        // En WAL, NORMAL ne perd jamais la cohérence de la base (au pire les dernières
        // transactions en cas de coupure) et évite un fsync par transaction de fichier.
        conn.pragma_update(None, "synchronous", "NORMAL")
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

    /// Ouvre en écriture un graphe existant, sans rien créer ni migrer : un schéma d'une autre
    /// version (ou une base vide) est refusé avec l'invitation à lancer `nephila build`.
    pub fn open_existing(repo: &Path) -> Result<Store> {
        let path = db_path(repo);
        if !path.is_file() {
            return Err(Error::Message(format!(
                "aucun graphe dans {} : lancez `nephila build`",
                repo.display()
            )));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        let store = Store {
            conn,
            read_only: false,
        };
        match store.user_version()? {
            v if v == SCHEMA_VERSION => Ok(store),
            v => Err(Error::Message(format!(
                "graphe de schéma {v} incompatible (attendu {SCHEMA_VERSION}) dans {} : lancez `nephila build`",
                path.display()
            ))),
        }
    }

    /// Ouvre le graphe en lecture seule, sans rien créer.
    pub fn open_readonly(repo: &Path) -> Result<Store> {
        let path = db_path(repo);
        if !path.is_file() {
            return Err(Error::Message(format!(
                "aucun graphe dans {} : lancez `nephila build`",
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
                    "graphe vide ou illisible dans {} : lancez `nephila build`",
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
                .prepare("INSERT INTO files(path, hash, size, mtime, recorded_at, state) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')")
                .map_err(db_err)?;
            for f in files {
                let size = i64::try_from(f.size)
                    .map_err(|_| Error::Message(format!("taille invalide pour {}", f.path)))?;
                st.execute(params![f.path, f.hash, size, f.mtime, now_ms()])
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
                    "INSERT INTO files(path, hash, size, mtime, recorded_at, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(path) DO UPDATE SET hash = excluded.hash, size = excluded.size,
                         mtime = excluded.mtime, recorded_at = excluded.recorded_at,
                         state = excluded.state",
                )
                .map_err(db_err)?;
            for f in files {
                let size = i64::try_from(f.size)
                    .map_err(|_| Error::Message(format!("taille invalide pour {}", f.path)))?;
                st.execute(params![
                    f.path,
                    f.hash,
                    size,
                    f.mtime,
                    now_ms(),
                    f.state.as_str()
                ])
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

    /// Alias de `replace_structure`.
    pub fn replace_file(&mut self, path: &str, nodes: &[Node], refs: &[Ref]) -> Result<()> {
        self.replace_structure(path, nodes, refs)
    }

    /// Remplace en une transaction tout ce qui vient d'un fichier (déclarations et références,
    /// origines `parse` et `llm`), efface les résumés de ses nœuds et remet `enriched_hash` à
    /// NULL, puis marque le fichier indexé. Le fichier doit être connu. Un nœud peut être déclaré
    /// par plusieurs fichiers (DEC-014) : la ligne du nœud (attributs, fichier de rattachement)
    /// est une projection des déclarations (DEC-026 : attributs et fichier de la déclaration de plus petit chemin, résumé de la plus petite déclaration résumée), les références restent attachées au fichier qui les a
    /// émises. Un nœud sans fichier (concept) est déclaré lui aussi par le fichier
    /// qui le mentionne et disparaît avec son dernier déclarant. Les nœuds `dependency` sont
    /// dérivés par la résolution (DEC-020) : ils ne sont jamais supprimés ici, seul
    /// `replace_derived_dependencies` les nettoie. Les références dont la source
    /// n'est pas déclarée par le fichier sont rejetées. Déclarations et références nouvelles
    /// ont l'origine `parse`.
    pub fn replace_structure(&mut self, path: &str, nodes: &[Node], refs: &[Ref]) -> Result<()> {
        self.replace_structures(&[(path.to_string(), nodes.to_vec(), refs.to_vec())])
    }

    /// Comme `replace_structure` pour plusieurs fichiers en une seule transaction (tout ou rien) :
    /// évite une validation par fichier lors d'un build de milliers de fichiers.
    pub fn replace_structures(&mut self, items: &[(String, Vec<Node>, Vec<Ref>)]) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        for (path, nodes, refs) in items {
            require_known(&tx, path)?;
            clear_summaries(&tx, path)?;
            release_file(&tx, path, None)?;
            insert_nodes(&tx, path, nodes, "parse")?;
            insert_refs(&tx, path, refs, "parse")?;
            tx.execute(
                "UPDATE files SET state = 'indexed', enriched_hash = NULL WHERE path = ?1",
                [path],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    /// Applique en une transaction le résultat de l'enrichissement d'un fichier : remplace ses
    /// déclarations et références d'origine `llm` (nœuds concept compris) et les résumés de ses
    /// nœuds, puis pose `enriched_hash = hash`. `summaries` : couples (identifiant, texte), les
    /// identifiants devant être déclarés par le fichier. Les références `llm` doivent partir d'un
    /// nœud déclaré par le fichier.
    pub fn apply_enrichment(
        &mut self,
        path: &str,
        hash: &str,
        summaries: &[(String, String)],
        concept_nodes: &[Node],
        refs: &[Ref],
    ) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        require_known(&tx, path)?;
        clear_summaries(&tx, path)?;
        release_file(&tx, path, Some("llm"))?;
        insert_nodes(&tx, path, concept_nodes, "llm")?;
        for (id, text) in summaries {
            let n = tx
                .execute(
                    "UPDATE declarations SET summary = ?2 WHERE node = ?1 AND file = ?3",
                    params![id, text, path],
                )
                .map_err(db_err)?;
            if n == 0 {
                return Err(Error::Message(format!(
                    "résumé pour le nœud {id}, non déclaré dans {path}"
                )));
            }
        }
        let ids = declared_by(&tx, path)?;
        refresh_nodes(&tx, &ids)?;
        insert_refs(&tx, path, refs, "llm")?;
        tx.execute(
            "UPDATE files SET enriched_hash = ?2 WHERE path = ?1",
            params![path, hash],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Fichiers indexés à enrichir : `enriched_hash` absent ou différent de `hash`.
    pub fn files_to_enrich(&self) -> Result<Vec<FileRecord>> {
        Ok(self
            .files()?
            .into_iter()
            .filter(|f| f.state == FileState::Indexed)
            .filter(|f| self.enriched_hash(&f.path).ok().flatten().as_deref() != Some(&f.hash))
            .collect())
    }

    /// Nombre de fichiers à enrichir.
    pub fn count_files_to_enrich(&self) -> Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE state = 'indexed'
                 AND (enriched_hash IS NULL OR enriched_hash <> hash)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(db_err)
    }

    /// Empreinte enrichie d'un fichier (`None` : jamais enrichi ou fichier inconnu).
    pub fn enriched_hash(&self, path: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT enriched_hash FROM files WHERE path = ?1",
            [path],
            |r| r.get::<_, Option<String>>(0),
        ) {
            Ok(h) => Ok(h),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Supprime un fichier, ses nœuds, références et les liens attachés. Renvoie `false` si inconnu.
    pub fn remove_file(&mut self, path: &str) -> Result<bool> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        clear_summaries(&tx, path)?;
        release_file(&tx, path, None)?;
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
        // Seule la différence avec les liens stockés est écrite : après une mise à jour, presque
        // tous les liens sont inchangés et les réécrire tous coûterait plusieurs secondes.
        let wanted: std::collections::HashSet<(&str, &str, &str)> = edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str(), e.kind.as_str()))
            .collect();
        let stale: Vec<(String, String, String)> = {
            let mut st = tx
                .prepare("SELECT source, target, kind FROM edges")
                .map_err(db_err)?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(db_err)?;
            let mut stale = Vec::new();
            let mut present: std::collections::HashSet<(String, String, String)> =
                std::collections::HashSet::new();
            for row in rows {
                let (s, t, k): (String, String, String) = row.map_err(db_err)?;
                if wanted.contains(&(s.as_str(), t.as_str(), k.as_str())) {
                    present.insert((s, t, k));
                } else {
                    stale.push((s, t, k));
                }
            }
            drop(st);
            let mut ins = tx
                .prepare("INSERT OR IGNORE INTO edges(source, target, kind) VALUES (?1, ?2, ?3)")
                .map_err(db_err)?;
            for e in edges {
                let key = (
                    e.source.clone(),
                    e.target.clone(),
                    e.kind.as_str().to_string(),
                );
                if !present.contains(&key) {
                    ins.execute(params![e.source, e.target, e.kind.as_str()])
                        .map_err(db_err)?;
                }
            }
            stale
        };
        let mut del = tx
            .prepare("DELETE FROM edges WHERE source = ?1 AND target = ?2 AND kind = ?3")
            .map_err(db_err)?;
        for (s, t, k) in &stale {
            del.execute(params![s, t, k]).map_err(db_err)?;
        }
        drop(del);
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

    /// Nœuds susceptibles de correspondre à un nom saisi : identifiant, nom qualifié ou nom court
    /// égal à `name`, ou nom qualifié terminé par `name`. Triés par identifiant. L'appelant
    /// affine (frontière de segment).
    pub fn nodes_matching(&self, name: &str) -> Result<Vec<Node>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes
                 WHERE id = ?1 OR qualified = ?1 OR short = ?1
                    OR (length(qualified) > length(?1) AND substr(qualified, -length(?1)) = ?1)
                 ORDER BY id",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map([name], |r| {
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

    /// Nœud d'identifiant `id`.
    pub fn node_by_id(&self, id: &str) -> Result<Option<Node>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes WHERE id = ?1",
            )
            .map_err(db_err)?;
        let row = st.query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        });
        match row {
            Ok((id, kind, qualified, short, file, subkind, summary)) => Ok(Some(Node {
                id,
                kind: NodeKind::parse(&kind)?,
                qualified,
                short,
                file,
                subkind: subkind.as_deref().map(SubKind::parse).transpose()?,
                summary,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Liens sortants (`outgoing`) ou entrants du nœud `id`, sous la forme (autre extrémité,
    /// genre), triés par identifiant de l'autre extrémité puis genre.
    pub fn edges_at(&self, id: &str, outgoing: bool) -> Result<Vec<(String, EdgeKind)>> {
        let sql = if outgoing {
            "SELECT target, kind FROM edges WHERE source = ?1 ORDER BY target, kind"
        } else {
            "SELECT source, kind FROM edges WHERE target = ?1 ORDER BY source, kind"
        };
        let mut st = self.conn.prepare_cached(sql).map_err(db_err)?;
        let rows = st
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (other, kind) = row.map_err(db_err)?;
            out.push((other, EdgeKind::parse(&kind)?));
        }
        Ok(out)
    }

    pub fn count_edges(&self) -> Result<usize> {
        self.count("edges")
    }

    pub fn count_nodes(&self) -> Result<usize> {
        self.count("nodes")
    }

    fn count(&self, table: &str) -> Result<usize> {
        self.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)
            .map_err(db_err)
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

    /// Références avec leur origine (`parse` ou `llm`), dans l'ordre d'insertion.
    pub fn refs_with_origin(&self) -> Result<Vec<(Ref, RefOrigin)>> {
        let mut st = self
            .conn
            .prepare("SELECT source, kind, target_name, context, origin FROM refs ORDER BY id")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (source, kind, target_name, context, origin) = row.map_err(db_err)?;
            let origin = if origin == "llm" {
                RefOrigin::Llm
            } else {
                RefOrigin::Parse
            };
            out.push((
                Ref {
                    source,
                    kind: EdgeKind::parse(&kind)?,
                    target_name,
                    context,
                },
                origin,
            ));
        }
        Ok(out)
    }

    /// Remplace les nœuds `dependency` dérivés par la résolution (sans déclaration, DEC-020) :
    /// ceux qui ne sont plus visés disparaissent, les autres sont créés ou conservés.
    pub fn replace_derived_dependencies(&mut self, deps: &[Node]) -> Result<()> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute(
            "DELETE FROM nodes WHERE kind = 'dependency'
             AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
            [],
        )
        .map_err(db_err)?;
        {
            let mut ins = tx
                .prepare(
                    "INSERT OR IGNORE INTO nodes(id, kind, qualified, short, file, subkind, summary)
                     VALUES (?1, 'dependency', ?2, ?3, NULL, NULL, NULL)",
                )
                .map_err(db_err)?;
            for n in deps {
                ins.execute(params![n.id, n.qualified, n.short])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
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

    /// Parmi `files`, ceux qui déclarent encore le nœud `document:<chemin>` (requête ciblée
    /// par clé primaire, sans charger toute la table des déclarations).
    pub fn files_declaring_document(&self, files: &[&str]) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT 1 FROM declarations WHERE file = ?1 AND node = ?2")
            .map_err(db_err)?;
        let mut found = Vec::new();
        for f in files {
            if st
                .exists(rusqlite::params![f, format!("document:{f}")])
                .map_err(db_err)?
            {
                found.push((*f).to_string());
            }
        }
        Ok(found)
    }

    pub fn files(&self) -> Result<Vec<FileRecord>> {
        let mut st = self
            .conn
            .prepare("SELECT path, hash, size, mtime, recorded_at, state FROM files ORDER BY path")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (path, hash, size, mtime, recorded_at, state) = row.map_err(db_err)?;
            out.push(FileRecord {
                path,
                hash,
                size: size.max(0) as u64,
                mtime,
                recorded_at,
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

fn require_known(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<()> {
    let known: bool = tx
        .query_row("SELECT 1 FROM files WHERE path = ?1", [path], |_| Ok(true))
        .unwrap_or(false);
    if known {
        Ok(())
    } else {
        Err(Error::Message(format!(
            "fichier inconnu du graphe : {path}"
        )))
    }
}

/// Efface les résumés posés par un fichier dans ses déclarations (ceux d'autres déclarants d'un
/// nœud partagé restent) puis recalcule les lignes des nœuds concernés.
fn clear_summaries(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<()> {
    tx.execute(
        "UPDATE declarations SET summary = NULL WHERE file = ?1 AND summary IS NOT NULL",
        [path],
    )
    .map_err(db_err)?;
    let ids = declared_by(tx, path)?;
    refresh_nodes(tx, &ids)
}

fn declared_by(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<Vec<String>> {
    let mut st = tx
        .prepare("SELECT node FROM declarations WHERE file = ?1")
        .map_err(db_err)?;
    let rows = st
        .query_map([path], |r| r.get::<_, String>(0))
        .map_err(db_err)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_err)
}

/// Recalcule la ligne des nœuds encore déclarés comme projection de leurs déclarations (DEC-026) :
/// attributs et fichier de la déclaration de plus petit chemin, résumé de la déclaration de plus
/// petit chemin ayant un résumé. Les nœuds non déclarés sont ignorés.
fn refresh_nodes<S: AsRef<str>>(tx: &rusqlite::Transaction<'_>, ids: &[S]) -> Result<()> {
    let mut up = tx
        .prepare(
            "UPDATE nodes SET
               short = (SELECT short FROM declarations WHERE node = nodes.id ORDER BY file LIMIT 1),
               subkind = (SELECT subkind FROM declarations WHERE node = nodes.id ORDER BY file LIMIT 1),
               file = (SELECT CASE WHEN located <> 0 THEN file END FROM declarations
                       WHERE node = nodes.id ORDER BY file LIMIT 1),
               summary = (SELECT summary FROM declarations
                          WHERE node = nodes.id AND summary IS NOT NULL ORDER BY file LIMIT 1),
               summary_file = (SELECT file FROM declarations
                          WHERE node = nodes.id AND summary IS NOT NULL ORDER BY file LIMIT 1)
             WHERE id = ?1 AND EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
        )
        .map_err(db_err)?;
    for id in ids {
        up.execute([id.as_ref()]).map_err(db_err)?;
    }
    Ok(())
}

fn insert_nodes(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    nodes: &[Node],
    origin: &str,
) -> Result<()> {
    let mut ins = tx
        .prepare("INSERT OR IGNORE INTO nodes(id, kind, qualified, short) VALUES (?1, ?2, ?3, ?4)")
        .map_err(db_err)?;
    let mut decl = tx
        .prepare(
            "INSERT OR IGNORE INTO declarations(file, node, origin, short, subkind, located, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(db_err)?;
    let mut ids: Vec<String> = Vec::new();
    for n in nodes {
        if n.file.as_deref().is_some_and(|f| f != path) {
            return Err(Error::Message(format!(
                "le nœud {} n'appartient pas au fichier {path}",
                n.id
            )));
        }
        ins.execute(params![n.id, n.kind.as_str(), n.qualified, n.short])
            .map_err(db_err)?;
        decl.execute(params![
            path,
            n.id,
            origin,
            n.short,
            n.subkind.map(SubKind::as_str),
            n.file.is_some(),
            n.summary
        ])
        .map_err(db_err)?;
        ids.push(n.id.clone());
    }
    refresh_nodes(tx, &ids)?;
    Ok(())
}

fn insert_refs(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    refs: &[Ref],
    origin: &str,
) -> Result<()> {
    let mut insr = tx
        .prepare(
            "INSERT INTO refs(source, file, kind, target_name, context, origin)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS
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
                r.context,
                origin
            ])
            .map_err(db_err)?;
        if n == 0 {
            return Err(Error::Message(format!(
                "référence dont la source {} n'est pas déclarée dans {path}",
                r.source
            )));
        }
    }
    Ok(())
}

/// Retire les déclarations et références d'un fichier (d'une seule origine si `origin` est
/// donnée), rattache ses nœuds partagés à un autre déclarant, et supprime les nœuds devenus
/// orphelins (liens attachés compris), sauf les dépendances dérivées par la résolution.
fn release_file(tx: &rusqlite::Transaction<'_>, path: &str, origin: Option<&str>) -> Result<()> {
    // Les nœuds `dependency` dérivés (sans déclaration, DEC-020) ne sont nettoyés que par
    // `replace_derived_dependencies` ; seuls ceux que ce fichier déclarait suivent ses déclarations.
    let released: Vec<String> = {
        let mut st = tx
            .prepare(
                "SELECT node FROM declarations WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![path, origin], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?
    };
    tx.execute(
        "DELETE FROM refs WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)",
        params![path, origin],
    )
    .map_err(db_err)?;
    tx.execute(
        "DELETE FROM declarations WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)",
        params![path, origin],
    )
    .map_err(db_err)?;
    refresh_nodes(tx, &released)?;
    let mut del = tx
        .prepare(
            "DELETE FROM nodes WHERE id = ?1
             AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
        )
        .map_err(db_err)?;
    for id in &released {
        del.execute([id]).map_err(db_err)?;
    }
    Ok(())
}

fn version_error(v: i32) -> Error {
    Error::Message(format!(
        "version de schéma du graphe non prise en charge ({v}, attendue {SCHEMA_VERSION}) : relancez `nephila build`"
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
            recorded_at: 0,
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
                "concept",
                "table",
                "key"
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
        assert_eq!(EdgeKind::ALL.len(), 9);
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
        // AC-005-2 (2026-10-17) : document de données et key → nœud de code, section → table.
        assert!(E::Mentions.allows(N::Document, N::Type));
        assert!(E::Mentions.allows(N::Document, N::Method));
        assert!(E::Mentions.allows(N::Key, N::Type));
        assert!(E::Mentions.allows(N::Section, N::Table));
        assert!(!E::Mentions.allows(N::Document, N::Table));
        assert!(!E::Mentions.allows(N::Document, N::Concept));
        assert!(!E::Mentions.allows(N::Key, N::Key));
        assert!(!E::Mentions.allows(N::Table, N::Type));
        assert!(E::Contains.allows(N::Document, N::Table));
        assert!(E::Contains.allows(N::Document, N::Key));
        assert!(!E::Contains.allows(N::Section, N::Key));
        assert!(!E::Contains.allows(N::Table, N::Key));
        assert!(E::References.allows(N::Table, N::Table));
        // Module ES (AC-005-2) : package -> type|method.
        assert!(E::Imports.allows(N::Package, N::Type));
        assert!(E::Imports.allows(N::Package, N::Method));
        assert!(!E::Imports.allows(N::Package, N::Package));
        assert!(!E::Imports.allows(N::Method, N::Type));
        assert!(!E::Extends.allows(N::Package, N::Type));
        assert!(E::Contains.allows(N::Package, N::Method));
        assert!(!E::Contains.allows(N::Method, N::Package));
        assert!(!E::References.allows(N::Table, N::Type));
        assert!(!E::References.allows(N::Type, N::Table));
        assert!(E::Calls.allows(N::Method, N::Method));
        assert!(!E::Calls.allows(N::Type, N::Method));
    }

    #[test]
    fn req005_ac1_genres_table_et_key() {
        assert_eq!(NodeKind::parse("table").unwrap(), NodeKind::Table);
        assert_eq!(NodeKind::parse("key").unwrap(), NodeKind::Key);
        assert!(!NodeKind::Table.is_code() && !NodeKind::Key.is_code());
        assert_eq!(NodeKind::ALL.len(), 10);
    }

    #[test]
    fn req005_ac4_subkinds_donnees() {
        for s in ["table", "view", "sql", "xml", "json", "yaml"] {
            assert_eq!(SubKind::parse(s).unwrap().as_str(), s);
        }
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
            "alias",
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
    fn req009_ac2_db_under_nephila_only_wal_and_version() {
        let dir = tempdir().unwrap();
        let st = Store::open_or_create(dir.path()).unwrap();
        assert!(dir.path().join(".nephila/graph.db").is_file());
        assert_eq!(st.journal_mode().unwrap(), "wal");
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
        drop(st);
        let top: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(top, [".nephila"]);
        // Réouverture d'une base existante.
        Store::open_or_create(dir.path()).unwrap();
    }

    #[test]
    fn req009_ac4_readonly_missing_creates_nothing() {
        let dir = tempdir().unwrap();
        let err = Store::open_readonly(dir.path()).err().unwrap();
        assert!(err.to_string().contains("nephila build"), "{err}");
        assert!(!dir.path().join(".nephila").exists());
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

    fn concept(name: &str) -> Node {
        Node::new(NodeKind::Concept, name, name, None, None, None)
    }
    fn mention(src: &str, target: &str) -> Ref {
        Ref {
            source: src.into(),
            kind: EdgeKind::Mentions,
            target_name: target.into(),
            context: None,
        }
    }
    fn origins(st: &Store, table: &str) -> Vec<String> {
        let mut q = st
            .conn
            .prepare(&format!("SELECT origin FROM {table} ORDER BY origin"))
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn req009_ac7_older_version_read_error_build_recreates() {
        let dir = tempdir().unwrap();
        drop(Store::open_or_create(dir.path()).unwrap());
        let c = Connection::open(db_path(dir.path())).unwrap();
        c.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
            .unwrap();
        drop(c);
        let e = Store::open_readonly(dir.path()).err().unwrap();
        assert!(e.to_string().contains("nephila build"), "{e}");
        let st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(st.files_to_enrich().unwrap().is_empty());
    }

    #[test]
    fn req024_ac7_replace_structure_erases_summaries_and_concepts() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        let a = ty("p.A", "a.md");
        st.replace_structure("a.md", std::slice::from_ref(&a), &[])
            .unwrap();
        assert_eq!(st.files_to_enrich().unwrap().len(), 1);
        st.apply_enrichment(
            "a.md",
            "h",
            &[("type:p.A".into(), "résumé".into())],
            &[concept("idée")],
            &[mention("type:p.A", "idée")],
        )
        .unwrap();
        assert!(st.files_to_enrich().unwrap().is_empty());
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
        assert_eq!(origins(&st, "refs"), vec!["llm"]);
        assert_eq!(origins(&st, "declarations"), vec!["llm", "parse"]);
        let n = st.nodes().unwrap();
        assert_eq!(n.len(), 2);
        assert_eq!(
            n.iter()
                .find(|n| n.id == "type:p.A")
                .unwrap()
                .summary
                .as_deref(),
            Some("résumé")
        );
        // Le fichier est réanalysé : résumés, concepts, références llm et marque disparaissent.
        st.replace_structure("a.md", &[a], &[]).unwrap();
        let n = st.nodes().unwrap();
        assert_eq!(n.len(), 1);
        assert!(n[0].summary.is_none());
        assert!(st.refs().unwrap().is_empty());
        assert_eq!(st.enriched_hash("a.md").unwrap(), None);
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
    }

    #[test]
    fn req024_ac7_shared_node_summary_from_other_file_is_kept() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md"), file("b.md")])
            .unwrap();
        let shared = concept("pkg");
        st.replace_structure("a.md", &[shared.clone(), ty("p.A", "a.md")], &[])
            .unwrap();
        st.replace_structure("b.md", &[shared, ty("p.B", "b.md")], &[])
            .unwrap();
        st.apply_enrichment(
            "b.md",
            "h",
            &[
                ("concept:pkg".into(), "partagé".into()),
                ("type:p.B".into(), "propre à b".into()),
            ],
            &[],
            &[],
        )
        .unwrap();
        st.replace_structure("a.md", &[concept("pkg"), ty("p.A", "a.md")], &[])
            .unwrap();
        let summary = |id: &str| {
            st.nodes()
                .unwrap()
                .into_iter()
                .find(|n| n.id == id)
                .unwrap()
                .summary
        };
        assert_eq!(summary("concept:pkg").as_deref(), Some("partagé"));
        assert_eq!(summary("type:p.B").as_deref(), Some("propre à b"));
    }

    #[test]
    fn req024_ac7_remove_file_clears_summary_of_shared_node() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md"), file("b.md")])
            .unwrap();
        let shared = concept("pkg");
        st.replace_structure("a.md", std::slice::from_ref(&shared), &[])
            .unwrap();
        st.replace_structure("b.md", std::slice::from_ref(&shared), &[])
            .unwrap();
        st.apply_enrichment(
            "a.md",
            "h",
            &[("concept:pkg".into(), "résumé".into())],
            &[],
            &[],
        )
        .unwrap();
        assert!(st.remove_file("a.md").unwrap());
        let n = st.nodes().unwrap();
        assert_eq!(n.len(), 1);
        assert!(n[0].summary.is_none());
    }

    #[test]
    fn dec014_shared_concept_removed_with_last_llm_declarer() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md"), file("b.md")])
            .unwrap();
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        st.replace_structure("b.md", &[ty("p.B", "b.md")], &[])
            .unwrap();
        for (f, s) in [("a.md", "type:p.A"), ("b.md", "type:p.B")] {
            st.apply_enrichment(f, "h", &[], &[concept("c")], &[mention(s, "c")])
                .unwrap();
        }
        assert_eq!(st.nodes().unwrap().len(), 3);
        // a.md est ré-enrichi sans le concept : b.md le déclare encore.
        st.apply_enrichment("a.md", "h", &[], &[], &[]).unwrap();
        assert!(st.nodes().unwrap().iter().any(|n| n.id == "concept:c"));
        assert_eq!(st.refs().unwrap().len(), 1);
        st.apply_enrichment("b.md", "h", &[], &[], &[]).unwrap();
        assert!(!st.nodes().unwrap().iter().any(|n| n.id == "concept:c"));
        assert_eq!(st.nodes().unwrap().len(), 2);
    }

    #[test]
    fn files_to_enrich_iff_enriched_hash_differs() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md"), file("b.md")])
            .unwrap();
        // En attente (illisible) : pas à enrichir.
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        let paths = |st: &Store| -> Vec<String> {
            st.files_to_enrich()
                .unwrap()
                .into_iter()
                .map(|f| f.path)
                .collect()
        };
        assert_eq!(paths(&st), vec!["a.md"]);
        st.apply_enrichment("a.md", "autre", &[], &[], &[]).unwrap();
        assert_eq!(paths(&st), vec!["a.md"]);
        st.apply_enrichment("a.md", "h", &[], &[], &[]).unwrap();
        assert!(paths(&st).is_empty());
        // Contenu modifié : l'empreinte diffère à nouveau.
        let mut f = file("a.md");
        f.hash = "h2".into();
        st.insert_files(&[f]).unwrap();
        assert_eq!(paths(&st), vec!["a.md"]);
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
    }

    #[test]
    fn apply_enrichment_rejects_foreign_summary_and_is_atomic() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        st.apply_enrichment(
            "a.md",
            "h",
            &[],
            &[concept("c")],
            &[mention("type:p.A", "c")],
        )
        .unwrap();
        let r = st.apply_enrichment(
            "a.md",
            "h2",
            &[("type:inconnu".into(), "x".into())],
            &[],
            &[],
        );
        assert!(r.is_err());
        assert_eq!(st.enriched_hash("a.md").unwrap().as_deref(), Some("h"));
        assert_eq!(st.refs().unwrap().len(), 1);
        assert!(st
            .apply_enrichment("absent.md", "h", &[], &[], &[])
            .is_err());
    }

    fn shared(f: &str, short: &str, sub: SubKind, sum: Option<&str>) -> Node {
        Node::new(NodeKind::Type, "p.A", short, Some(f), Some(sub), sum)
    }

    fn shared_store(order: [&str; 2]) -> (tempfile::TempDir, Store) {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a"), file("b")]).unwrap();
        for f in order {
            let n = if f == "a" {
                shared("a", "Aa", SubKind::Class, None)
            } else {
                shared("b", "Bb", SubKind::Interface, None)
            };
            st.replace_structure(f, &[n], &[]).unwrap();
        }
        (dir, st)
    }

    #[test]
    fn shared_node_row_is_independent_of_insertion_order() {
        let (_d1, ab) = shared_store(["a", "b"]);
        let (_d2, ba) = shared_store(["b", "a"]);
        assert_eq!(ab.nodes().unwrap(), ba.nodes().unwrap());
        let n = &ab.nodes().unwrap()[0];
        assert_eq!(n.short, "Aa");
        assert_eq!(n.subkind, Some(SubKind::Class));
        assert_eq!(n.file.as_deref(), Some("a"));
        let (_d3, mut st) = shared_store(["b", "a"]);
        st.remove_file("a").unwrap();
        let n = &st.nodes().unwrap()[0];
        assert_eq!(n.short, "Bb");
        assert_eq!(n.subkind, Some(SubKind::Interface));
        assert_eq!(n.file.as_deref(), Some("b"));
    }

    #[test]
    fn shared_summaries_follow_smallest_path_in_both_orders() {
        for order in [["a", "b"], ["b", "a"]] {
            let (_d, mut st) = shared_store(order);
            for f in order {
                st.apply_enrichment(f, "h", &[("type:p.A".into(), format!("res-{f}"))], &[], &[])
                    .unwrap();
            }
            let n = &st.nodes().unwrap()[0];
            assert_eq!(n.summary.as_deref(), Some("res-a"));
            // Réanalyse de `a` : son résumé disparaît, celui de `b` apparaît.
            st.replace_structure("a", &[shared("a", "Aa", SubKind::Class, None)], &[])
                .unwrap();
            assert_eq!(st.nodes().unwrap()[0].summary.as_deref(), Some("res-b"));
        }
    }

    #[test]
    fn shared_concept_has_no_file_and_smallest_path_summary() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md"), file("b.md")])
            .unwrap();
        for f in ["b.md", "a.md"] {
            st.replace_structure(f, &[], &[]).unwrap();
            let c = Node::new(NodeKind::Concept, "c", "c", None, None, None);
            st.apply_enrichment(f, "h", &[("concept:c".into(), format!("r-{f}"))], &[c], &[])
                .unwrap();
        }
        let n = &st.nodes().unwrap()[0];
        assert_eq!(n.file, None);
        assert_eq!(n.summary.as_deref(), Some("r-a.md"));
    }
}
