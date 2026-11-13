//! Fichier d'enrichissements (REQ-295, DEC-226) : une base SQLite qui ne contient que la table
//! `enrichments` du graphe et une table `meta` (`format`, `version`).

use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OpenFlags};

use super::{db_err, Store, ENRICHMENTS_DDL};
use crate::error::{Error, Result};

/// Valeur de `meta.format`.
const FORMAT: &str = "nephila-enrichments";
/// Valeur de `meta.version`.
const VERSION: i64 = 1;
/// Colonnes de `enrichments` (schéma 17), dans l'ordre du schéma.
const COLUMNS: [&str; 9] = [
    "path",
    "content_hash",
    "model",
    "prompt_version",
    "payload",
    "language",
    "used_at",
    "served_model",
    "nodes_hash",
];

fn columns_sql() -> String {
    COLUMNS.join(", ")
}

fn write_error(file: &Path, cause: impl std::fmt::Display) -> Error {
    Error::Message(format!(
        "écriture de {} impossible : {cause}",
        file.display()
    ))
}

fn not_ours(file: &Path) -> Error {
    Error::Message(format!(
        "{} : ce fichier n'a pas été écrit par nephila cache export",
        file.display()
    ))
}

/// Ouvre le fichier en lecture seule et le contrôle (E-38, E-39, E-40) ; jamais modifié.
fn open_checked(file: &Path) -> Result<Connection> {
    let unreadable = |e: std::io::Error| {
        Error::Message(format!("{} : lecture impossible : {e}", file.display()))
    };
    let mut f = std::fs::File::open(file).map_err(unreadable)?;
    // Un dossier s'ouvre, mais ne se lit pas.
    let mut first = [0u8; 1];
    f.read(&mut first).map_err(unreadable)?;
    drop(f);
    let conn = Connection::open_with_flags(
        file,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| not_ours(file))?;
    let meta = |key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    };
    if meta("format").as_deref() != Some(FORMAT) {
        return Err(not_ours(file));
    }
    let version: i64 = meta("version")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| not_ours(file))?;
    if version != VERSION {
        return Err(Error::Message(format!(
            "{} : version {version} du fichier non prise en charge (attendue {VERSION}) ; relancez nephila cache export avec cette version de nephila",
            file.display()
        )));
    }
    let names: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('enrichments') ORDER BY cid")
        .and_then(|mut q| q.query_map([], |r| r.get(0))?.collect())
        .map_err(|_| not_ours(file))?;
    if names != COLUMNS {
        return Err(not_ours(file));
    }
    Ok(conn)
}

/// Lignes de `enrichments` d'une connexion, triées par clé.
fn read_rows(conn: &Connection) -> Result<Vec<Vec<Value>>> {
    let sql = format!(
        "SELECT {} FROM enrichments ORDER BY path, content_hash, model, prompt_version, language",
        columns_sql()
    );
    let mut q = conn.prepare(&sql).map_err(db_err)?;
    let rows = q
        .query_map([], |r| {
            (0..COLUMNS.len()).map(|i| r.get::<_, Value>(i)).collect()
        })
        .map_err(db_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(rows)
}

fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

impl Store {
    /// Écrit toutes les entrées d'enrichissement du graphe dans `target` (remplacé s'il existe) ;
    /// rend leur nombre.
    pub fn export_enrichments(&self, target: &Path) -> Result<usize> {
        let tmp = temp_path(target);
        // Fichier neuf : l'erreur du système (dossier absent, droits) est celle que l'on montre.
        // Rien n'est à supprimer si la création échoue.
        std::fs::File::create_new(&tmp).map_err(|e| write_error(target, e))?;
        let result = self.write_cache_file(&tmp, target).and_then(|n| {
            std::fs::rename(&tmp, target).map_err(|e| write_error(target, e))?;
            Ok(n)
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn write_cache_file(&self, tmp: &Path, target: &Path) -> Result<usize> {
        let rows = read_rows(&self.conn)?;
        let mut out = Connection::open(tmp).map_err(|e| write_error(target, e))?;
        let tx = out.transaction().map_err(|e| write_error(target, e))?;
        tx.execute_batch(&format!(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\n{ENRICHMENTS_DDL}"
        ))
        .map_err(|e| write_error(target, e))?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES ('format', ?1), ('version', ?2)",
            (FORMAT, VERSION.to_string()),
        )
        .map_err(|e| write_error(target, e))?;
        let insert = format!(
            "INSERT INTO enrichments ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            columns_sql()
        );
        for row in &rows {
            tx.execute(&insert, params_from_iter(row))
                .map_err(|e| write_error(target, e))?;
        }
        tx.commit().map_err(|e| write_error(target, e))?;
        out.close().map_err(|(_, e)| write_error(target, e))?;
        Ok(rows.len())
    }

    /// Contrôle un fichier d'enrichissements sans rien écrire (E-38, E-39, E-40).
    pub fn check_enrichments_file(file: &Path) -> Result<()> {
        open_checked(file).map(|_| ())
    }

    /// Ajoute les entrées du fichier au graphe ; une entrée de même clé déjà présente est gardée.
    /// Rend (ajoutées, déjà présentes).
    pub fn import_enrichments(&mut self, file: &Path) -> Result<(usize, usize)> {
        self.ensure_writable()?;
        let source = open_checked(file)?;
        let rows = read_rows(&source)?;
        self.end_fresh();
        let tx = self.conn.transaction().map_err(db_err)?;
        let insert = format!(
            "INSERT INTO enrichments ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT DO NOTHING",
            columns_sql()
        );
        let mut added = 0;
        for row in &rows {
            added += tx.execute(&insert, params_from_iter(row)).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok((added, rows.len() - added))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Graphe peuplé : trois entrées, dont une `served_model` NULL et une `nodes_hash` NULL.
    fn graph(repo: &Path) -> Store {
        let st = Store::open_or_create(repo).unwrap();
        st.conn
            .execute_batch(
                "INSERT INTO enrichments VALUES ('a.java','h1','m',7,'{\"p\":1}','français',10,'m-2024','nh1');
                 INSERT INTO enrichments VALUES ('b.java','h2','m',7,'{\"p\":2}','français',20,NULL,'nh2');
                 INSERT INTO enrichments VALUES ('c.java','h3','n',8,'{\"p\":3}','english',30,'n-x',NULL);",
            )
            .unwrap();
        drop(st);
        Store::open_readonly(repo).unwrap()
    }

    fn select_all(conn: &Connection, sql: &str) -> Vec<Vec<Value>> {
        let mut q = conn.prepare(sql).unwrap();
        let n = q.column_count();
        q.query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect())
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn table(conn: &Connection) -> Vec<Vec<Value>> {
        read_rows(conn).unwrap()
    }

    fn payload_of(st: &Store, path: &str) -> (String, i64) {
        st.conn
            .query_row(
                "SELECT payload, used_at FROM enrichments WHERE path = ?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn hash(path: &Path) -> String {
        blake3::hash(&std::fs::read(path).unwrap())
            .to_hex()
            .to_string()
    }

    #[test]
    fn req295_ac2_a_export_matches_graph_column_by_column() {
        let repo = tempdir().unwrap();
        let out = tempdir().unwrap();
        let st = graph(repo.path());
        let file = out.path().join("e.db");
        assert_eq!(st.export_enrichments(&file).unwrap(), 3);
        let conn = Connection::open(&file).unwrap();
        let rows = table(&conn);
        let sql = "SELECT * FROM enrichments ORDER BY path, content_hash, model, prompt_version, language";
        let graph_rows = select_all(&st.conn, sql);
        assert_eq!(rows, graph_rows);
        assert_eq!(select_all(&conn, sql), graph_rows);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|r| r[7] == Value::Null));
        assert!(rows.iter().any(|r| r[8] == Value::Null));
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(tables, ["enrichments", "meta"]);
        let meta: Vec<(String, String)> = conn
            .prepare("SELECT key, value FROM meta ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            meta,
            [
                ("format".to_string(), "nephila-enrichments".to_string()),
                ("version".to_string(), "1".to_string())
            ]
        );
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_ne!(mode, "wal");
        assert!(!out.path().join("e.db-wal").exists());
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 1);
    }

    #[test]
    fn req295_ac2_b_c_import_keeps_local_entry_and_is_idempotent() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        let out = tempdir().unwrap();
        // Fichier : K1 (charge P') et K2.
        let source = Store::open_or_create(src.path()).unwrap();
        source
            .conn
            .execute_batch(
                "INSERT INTO enrichments VALUES ('a.java','h1','m',7,'{\"p\":\"P'')','français',10,'m-2024','nh1');
                 INSERT INTO enrichments VALUES ('b.java','h2','m',7,'{\"p\":2}','français',20,NULL,'nh2');",
            )
            .unwrap();
        drop(source);
        let file = out.path().join("e.db");
        Store::open_readonly(src.path())
            .unwrap()
            .export_enrichments(&file)
            .unwrap();
        // Graphe : K1 de charge P et d'un autre used_at.
        let local = Store::open_or_create(dst.path()).unwrap();
        local
            .conn
            .execute(
                "INSERT INTO enrichments VALUES ('a.java','h1','m',7,'{\"p\":\"P\"}','français',99,NULL,NULL)",
                [],
            )
            .unwrap();
        drop(local);
        let mut dst_store = Store::open_or_create(dst.path()).unwrap();
        assert_eq!(dst_store.import_enrichments(&file).unwrap(), (1, 1));
        assert_eq!(
            payload_of(&dst_store, "a.java"),
            ("{\"p\":\"P\"}".to_string(), 99)
        );
        let k2 = "SELECT * FROM enrichments WHERE path = 'b.java'";
        let from_file = select_all(&Connection::open(&file).unwrap(), k2);
        assert_eq!(from_file.len(), 1);
        assert_eq!(select_all(&dst_store.conn, k2), from_file);
        let after_first = table(&dst_store.conn);
        assert_eq!(after_first.len(), 2);
        assert_eq!(dst_store.import_enrichments(&file).unwrap(), (0, 2));
        assert_eq!(table(&dst_store.conn), after_first);
    }

    #[test]
    fn req295_ac2_d_export_replaces_existing_file_and_cleans_up_on_error() {
        let repo = tempdir().unwrap();
        let out = tempdir().unwrap();
        let st = graph(repo.path());
        let fresh = out.path().join("fresh.db");
        st.export_enrichments(&fresh).unwrap();
        let file = out.path().join("e.db");
        std::fs::write(&file, "ancien contenu").unwrap();
        st.export_enrichments(&file).unwrap();
        assert_eq!(
            table(&Connection::open(&file).unwrap()),
            table(&Connection::open(&fresh).unwrap())
        );
        let missing = out.path().join("absent").join("e.db");
        let err = st.export_enrichments(&missing).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "écriture de {} impossible : No such file or directory (os error 2)",
                missing.display()
            )
        );
        assert!(!out.path().join("absent").exists());
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 2);
    }

    #[test]
    fn req295_read_errors_e38_e39_e40() {
        let repo = tempdir().unwrap();
        let out = tempdir().unwrap();
        let st = graph(repo.path());
        let name = |f: &str| out.path().join(f);

        let absent = name("absent.db");
        let err = Store::check_enrichments_file(&absent)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "{} : lecture impossible : No such file or directory (os error 2)",
                absent.display()
            )
        );
        let dir = out.path().to_path_buf();
        let err = Store::check_enrichments_file(&dir).unwrap_err().to_string();
        assert!(err.contains("lecture impossible : Is a directory"), "{err}");

        let not_ours = |p: &Path| {
            format!(
                "{} : ce fichier n'a pas été écrit par nephila cache export",
                p.display()
            )
        };
        let text = name("t.txt");
        std::fs::write(&text, "bonjour, ceci n'est pas une base").unwrap();
        let empty = name("empty.db");
        std::fs::write(&empty, "").unwrap();
        let no_meta = name("nometa.db");
        Connection::open(&no_meta)
            .unwrap()
            .execute_batch("CREATE TABLE autre (x)")
            .unwrap();
        let mut bad: Vec<PathBuf> = vec![text, empty, no_meta];
        let mutate = |file: &str, sql: &str| {
            let good = name("good.db");
            if !good.exists() {
                st.export_enrichments(&good).unwrap();
            }
            let p = name(file);
            std::fs::copy(&good, &p).unwrap();
            Connection::open(&p).unwrap().execute_batch(sql).unwrap();
            p
        };
        bad.push(mutate(
            "fmt.db",
            "UPDATE meta SET value = 'autre' WHERE key = 'format'",
        ));
        bad.push(mutate(
            "ver.db",
            "UPDATE meta SET value = 'x' WHERE key = 'version'",
        ));
        bad.push(mutate("noenr.db", "DROP TABLE enrichments"));
        bad.push(mutate(
            "cols.db",
            "ALTER TABLE enrichments DROP COLUMN nodes_hash",
        ));
        for p in &bad {
            let before = hash(p);
            let err = Store::check_enrichments_file(p).unwrap_err().to_string();
            assert_eq!(err, not_ours(p));
            assert_eq!(hash(p), before);
        }

        let v2 = mutate("v2.db", "UPDATE meta SET value = '2' WHERE key = 'version'");
        let before = hash(&v2);
        let err = Store::check_enrichments_file(&v2).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "{} : version 2 du fichier non prise en charge (attendue 1) ; relancez nephila cache export avec cette version de nephila",
                v2.display()
            )
        );
        assert_eq!(hash(&v2), before);

        // Un bon fichier passe, et l'import d'un mauvais ne touche pas au graphe.
        Store::check_enrichments_file(&name("good.db")).unwrap();
        let dst = tempdir().unwrap();
        let mut dst_store = Store::open_or_create(dst.path()).unwrap();
        assert!(dst_store.import_enrichments(&v2).is_err());
        assert!(table(&dst_store.conn).is_empty());
    }
}
