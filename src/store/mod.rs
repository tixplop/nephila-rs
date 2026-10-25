//! Stockage SQLite du graphe dans `<dépôt>/.nephila/graph.db` (DEC-009, DEC-010).

use std::borrow::Borrow;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::llm::enrich::ENRICH_PROMPT_VERSION;

use crate::error::{Error, Result};

pub(crate) mod prepare;
use crate::model::{
    Edge, EdgeKind, FileRecord, FileState, Node, NodeKind, Ref, RefOrigin, SubKind,
};
#[cfg(test)]
pub(crate) use prepare::{preparations_on_thread, preparations_total};
pub use prepare::{prepare_raw, prepare_structure, PreparedRef, PreparedStructure};

/// Instant courant en millisecondes depuis l'epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Version du schéma, stockée dans `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 8;

/// Dernier schéma sans `qualified_rev`, migré sur place (DEC-066).
const SCHEMA_WITHOUT_REV: i32 = 5;

/// Dernier schéma avec `refs.context`, `AUTOINCREMENT` et les index allégés au schéma 7, migré
/// sur place (DEC-073).
const SCHEMA_WITH_REF_CONTEXT: i32 = 6;

/// Dernier schéma sans clé, tête ni statut des références ni marqueur `meta.resolution`, migré
/// sur place (DEC-076).
const SCHEMA_WITH_FILE_CONTEXTS: i32 = 7;

/// Valeurs de `meta.resolution` (DEC-076) : le graphe est-il le résultat d'une passe terminée ?
const RESOLUTION_DONE: &str = "à jour";
const RESOLUTION_PENDING: &str = "à résoudre";

/// Références du schéma 7 : le contexte d'une référence d'analyse est celui de son fichier.
const REFS_SQL: &str = "SELECT r.source, r.kind, r.target_name,
        CASE WHEN r.origin = 'parse' THEN c.context END, r.origin
     FROM refs r LEFT JOIN file_contexts c ON c.file = r.file ORDER BY r.id";
/// Références des schémas 5 et 6 (0.20.0), pour l'ouverture en lecture seule.
const REFS_LEGACY_SQL: &str =
    "SELECT source, kind, target_name, context, origin FROM refs ORDER BY id";

/// Références avec fichier et origine, sans contexte (REQ-063).
const FILE_REFS_SQL: &str =
    "SELECT id, source, kind, target_name, origin, file FROM refs ORDER BY id";
/// Contextes d'imports par fichier du schéma 7.
const FILE_CONTEXTS_SQL: &str = "SELECT file, context FROM file_contexts";
/// Contextes des schémas 5 et 6, regroupés par fichier (lecture seule).
const FILE_CONTEXTS_LEGACY_SQL: &str = "SELECT file, min(context) FROM refs
     WHERE origin = 'parse' AND context IS NOT NULL GROUP BY file";

/// Référence avec son fichier et son origine ; le contexte d'imports est lu à part
/// (`Store::file_contexts`), une fois par fichier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    /// Identifiant de la référence (`refs.id`).
    pub id: i64,
    /// Référence, `context` à `None`.
    pub r: Ref,
    pub origin: RefOrigin,
    pub file: String,
}

/// Requêtes de résolution d'un nom (DEC-066), toutes sur index.
pub(crate) const RESOLVE_ID: &str =
    "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes WHERE id = ?1";
pub(crate) const RESOLVE_QUALIFIED: &str =
    "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes WHERE qualified = ?1";
pub(crate) const RESOLVE_SHORT: &str =
    "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes WHERE short = ?1";
/// Suffixe : `?1` = inverse du nom suivi du caractère de frontière, `?2` = le même avec le
/// caractère suivant (comparaison binaire = ordre des points de code en UTF-8).
pub(crate) const RESOLVE_SUFFIX: &str =
    "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes
     WHERE qualified_rev >= ?1 AND qualified_rev < ?2";
/// Requête de la 0.18.0, pour un schéma 5 ouvert en lecture seule.
pub(crate) const RESOLVE_LEGACY: &str =
    "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes
     WHERE id = ?1 OR qualified = ?1 OR short = ?1
        OR (length(qualified) > length(?1) AND substr(qualified, -length(?1)) = ?1)
     ORDER BY id";
/// Caractères de frontière de segment et leur successeur immédiat.
const RESOLVE_BOUNDARIES: [(char, char); 4] = [('.', '/'), ('#', '$'), ('/', '0'), (':', ';')];

const SCHEMA: &str = include_str!("schema.sql");

/// Requêtes de la passe de résolution incrémentale (DEC-077), partagées avec le test d'AC-066-4
/// (`EXPLAIN QUERY PLAN`) : toutes sur index, sauf le chargement des nœuds de l'index de
/// résolution et les deux comptes (agrégats calculés par SQLite sur des index).
pub(crate) const RESOLVE_NODES: &str = "SELECT id, kind, qualified, short, file FROM nodes";
pub(crate) const REF_SOURCES_BY_KEY: &str =
    "SELECT DISTINCT source FROM refs WHERE target_key = ?1";
pub(crate) const REF_SOURCES_BY_HEAD: &str =
    "SELECT DISTINCT source FROM refs WHERE target_head = ?1";
pub(crate) const REF_SOURCES_BY_FILE: &str = "SELECT DISTINCT source FROM refs WHERE file = ?1";
pub(crate) const REFS_OF_SOURCE: &str = "SELECT id, kind, target_name, origin, file, unresolved
     FROM refs WHERE source = ?1 ORDER BY id";
pub(crate) const FILE_CONTEXT: &str = "SELECT context FROM file_contexts WHERE file = ?1";
pub(crate) const DECLARING_FILES: &str = "SELECT file FROM declarations WHERE node = ?1";
pub(crate) const DECLARED_NODES: &str = "SELECT node FROM declarations WHERE file = ?1";
pub(crate) const STORED_REF_EDGES: &str =
    "SELECT target, kind FROM edges WHERE source = ?1 AND kind <> 'contains'";
pub(crate) const STORED_CONTAINS: &str =
    "SELECT source FROM edges WHERE target = ?1 AND kind = 'contains'";
pub(crate) const INSERT_EDGE: &str =
    "INSERT OR IGNORE INTO edges(source, target, kind) VALUES (?1, ?2, ?3)";
pub(crate) const DELETE_EDGE: &str =
    "DELETE FROM edges WHERE source = ?1 AND target = ?2 AND kind = ?3";
pub(crate) const INSERT_DEPENDENCY: &str =
    "INSERT OR IGNORE INTO nodes(id, kind, qualified, short, file, subkind, summary, qualified_rev)
     VALUES (?1, 'dependency', ?2, ?3, NULL, NULL, NULL, ?4)";
pub(crate) const DEPENDS_ON_ENTERING: &str =
    "SELECT 1 FROM edges WHERE target = ?1 AND kind = 'depends_on' LIMIT 1";
/// Sources des liens `depends_on` qui visent une cible (`idx_edges_target`).
pub(crate) const DEPENDS_ON_SOURCES: &str =
    "SELECT source FROM edges WHERE target = ?1 AND kind = 'depends_on'";
pub(crate) const DELETE_DEPENDENCY: &str = "DELETE FROM nodes WHERE id = ?1 AND kind = 'dependency'
     AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = ?1)";
pub(crate) const SET_UNRESOLVED: &str = "UPDATE refs SET unresolved = ?2 WHERE id = ?1";
pub(crate) const COUNT_EDGES: &str = "SELECT COUNT(*) FROM edges";
pub(crate) const COUNT_UNRESOLVED: &str = "SELECT count(*) FROM refs WHERE unresolved = 1";
/// Liens `depends_on` sortants d'un module (clé primaire).
pub(crate) const SUMMARY_DEPENDS_ON: &str =
    "SELECT target FROM edges WHERE source = ?1 AND kind = 'depends_on'";
/// Les dix nœuds de plus haut degré (agrégat sur les deux index couvrants de `edges` ; un lien
/// d'un nœud vers lui-même compte deux fois), égalités départagées par identifiant.
pub(crate) const SUMMARY_TOP_DEGREES: &str = "SELECT id, SUM(n) AS degree FROM (
         SELECT source AS id, COUNT(*) AS n FROM edges GROUP BY source
         UNION ALL
         SELECT target AS id, COUNT(*) AS n FROM edges GROUP BY target)
     GROUP BY id ORDER BY degree DESC, id LIMIT ?1";
/// Nombre de mentions de chaque concept mentionné (intervalle `concept:` de `idx_edges_target`).
pub(crate) const SUMMARY_MENTIONS: &str = "SELECT target, COUNT(*) FROM edges
     WHERE target >= 'concept:' AND target < 'concept;' AND kind = 'mentions' GROUP BY target";
/// Toutes les requêtes ci-dessus, nommées.
#[cfg(test)]
pub(crate) const INCREMENTAL_QUERIES: [(&str, &str); 22] = [
    ("RESOLVE_NODES", RESOLVE_NODES),
    ("REF_SOURCES_BY_KEY", REF_SOURCES_BY_KEY),
    ("REF_SOURCES_BY_HEAD", REF_SOURCES_BY_HEAD),
    ("REF_SOURCES_BY_FILE", REF_SOURCES_BY_FILE),
    ("REFS_OF_SOURCE", REFS_OF_SOURCE),
    ("FILE_CONTEXT", FILE_CONTEXT),
    ("DECLARING_FILES", DECLARING_FILES),
    ("DECLARED_NODES", DECLARED_NODES),
    ("STORED_REF_EDGES", STORED_REF_EDGES),
    ("STORED_CONTAINS", STORED_CONTAINS),
    ("INSERT_EDGE", INSERT_EDGE),
    ("DELETE_EDGE", DELETE_EDGE),
    ("INSERT_DEPENDENCY", INSERT_DEPENDENCY),
    ("DEPENDS_ON_ENTERING", DEPENDS_ON_ENTERING),
    ("DEPENDS_ON_SOURCES", DEPENDS_ON_SOURCES),
    ("DELETE_DEPENDENCY", DELETE_DEPENDENCY),
    ("SET_UNRESOLVED", SET_UNRESOLVED),
    ("COUNT_EDGES", COUNT_EDGES),
    ("COUNT_UNRESOLVED", COUNT_UNRESOLVED),
    ("SUMMARY_DEPENDS_ON", SUMMARY_DEPENDS_ON),
    ("SUMMARY_TOP_DEGREES", SUMMARY_TOP_DEGREES),
    ("SUMMARY_MENTIONS", SUMMARY_MENTIONS),
];

/// Ce qu'une passe incrémentale écrit (DEC-077 point 3), calculé par `resolve`.
#[derive(Debug, Default)]
pub(crate) struct IncrementalWrite {
    /// Nœuds `dependency` visés par les `depends_on` réévalués (créés s'ils manquent).
    pub dependencies: Vec<Node>,
    /// Par source réévaluée : ses liens sortants voulus, hors `contains`.
    pub sources: Vec<(String, Vec<Edge>)>,
    /// Par cible réévaluée : ses liens `contains` entrants voulus.
    pub contained: Vec<(String, Vec<Edge>)>,
    /// Statut des références dont il change (identifiant, non résolue).
    pub statuses: Vec<(i64, bool)>,
}

/// Nom qualifié aux caractères Unicode inversés : valeur de `nodes.qualified_rev`. Seule
/// fonction de calcul, appelée à chaque écriture d'un nœud.
pub fn reverse_qualified(qualified: &str) -> String {
    qualified.chars().rev().collect()
}

/// Capacité du cache de requêtes préparées de chaque connexion (REQ-061).
const STATEMENT_CACHE_CAPACITY: usize = 64;

/// Attente maximale d'un verrou SQLite.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Réglages communs des ouvertures en écriture (REQ-059), posés avant tout contrôle de version
/// ou migration. En WAL, `synchronous = NORMAL` ne perd jamais la cohérence de la base (au pire
/// les dernières transactions en cas de coupure) et évite un fsync par transaction.
fn configure_write(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT).map_err(db_err)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(db_err)?;
    conn.pragma_update(None, "cache_size", -262_144)
        .map_err(db_err)?;
    conn.pragma_update(None, "temp_store", "MEMORY")
        .map_err(db_err)?;
    conn.pragma_update(None, "mmap_size", 268_435_456_i64)
        .map_err(db_err)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(db_err)?;
    Ok(())
}

thread_local! {
    static COMMITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PROJECTIONS: std::cell::RefCell<std::collections::HashMap<String, u64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Lignes de `nodes`, lignes de `edges`, passes complètes, passes incrémentales, références
    /// réévaluées (REQ-066, hypothèse « mesure »).
    static READS: std::cell::Cell<[u64; 5]> = const { std::cell::Cell::new([0; 5]) };
}

const NODE_ROWS: usize = 0;
const EDGE_ROWS: usize = 1;
const FULL_PASSES: usize = 2;
const INCREMENTAL_PASSES: usize = 3;
const REFS_REEVALUATED: usize = 4;

/// Ajoute `n` au compteur de diagnostic `slot`.
fn bump(slot: usize, n: usize) {
    READS.with(|c| {
        let mut v = c.get();
        v[slot] += n as u64;
        c.set(v);
    });
}

/// Compte des lignes de `nodes` rendues par une lecture du stockage (lecture de la passe
/// comprise ; un agrégat ne compte pas).
pub(crate) fn note_node_rows(n: usize) {
    bump(NODE_ROWS, n);
}

/// Compte des lignes de `edges` rendues par une lecture du stockage.
pub(crate) fn note_edge_rows(n: usize) {
    bump(EDGE_ROWS, n);
}

/// Une passe de résolution a eu lieu (`incremental` : passe incrémentale, sinon complète).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn note_pass(incremental: bool) {
    bump(
        if incremental {
            INCREMENTAL_PASSES
        } else {
            FULL_PASSES
        },
        1,
    );
}

/// `n` références réévaluées par une passe.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn note_refs_reevaluated(n: usize) {
    bump(REFS_REEVALUATED, n);
}

/// Compteurs de diagnostic du fil courant, lus par les tests seulement.
#[doc(hidden)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    commits: u64,
    projections: std::collections::HashMap<String, u64>,
    counters: [u64; 5],
}

impl Diagnostics {
    /// Transactions d'écriture du `Store` validées depuis la dernière remise à zéro.
    pub fn commits(&self) -> u64 {
        self.commits
    }

    /// Nombre de projections exécutées pour le nœud `id`.
    pub fn projections(&self, id: &str) -> u64 {
        self.projections.get(id).copied().unwrap_or(0)
    }

    /// Nombre total de projections exécutées.
    pub fn total_projections(&self) -> u64 {
        self.projections.values().sum()
    }

    /// Lignes de la table `nodes` rendues par les lectures du stockage.
    pub fn node_rows_read(&self) -> u64 {
        self.counters[NODE_ROWS]
    }

    /// Lignes de la table `edges` rendues par les lectures du stockage.
    pub fn edge_rows_read(&self) -> u64 {
        self.counters[EDGE_ROWS]
    }

    /// Passes de résolution complètes.
    pub fn full_passes(&self) -> u64 {
        self.counters[FULL_PASSES]
    }

    /// Passes de résolution incrémentales.
    pub fn incremental_passes(&self) -> u64 {
        self.counters[INCREMENTAL_PASSES]
    }

    /// Références réévaluées par les passes.
    pub fn refs_reevaluated(&self) -> u64 {
        self.counters[REFS_REEVALUATED]
    }
}

#[doc(hidden)]
pub fn diagnostics() -> Diagnostics {
    Diagnostics {
        commits: COMMITS.with(std::cell::Cell::get),
        projections: PROJECTIONS.with(|p| p.borrow().clone()),
        counters: READS.with(std::cell::Cell::get),
    }
}

#[doc(hidden)]
pub fn reset_diagnostics() {
    COMMITS.with(|c| c.set(0));
    PROJECTIONS.with(|p| p.borrow_mut().clear());
    READS.with(|c| c.set([0; 5]));
}

/// Valide une transaction d'écriture et la compte.
fn commit_counted(tx: rusqlite::Transaction<'_>) -> Result<()> {
    tx.commit().map_err(db_err)?;
    COMMITS.with(|c| c.set(c.get() + 1));
    Ok(())
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::Message(format!("base du graphe : {e}"))
}

/// Chemin du fichier de base pour un dépôt.
pub fn db_path(repo: &Path) -> PathBuf {
    repo.join(".nephila").join("graph.db")
}

/// Chemins examinés par `ensure_no_symlink`, relatifs au dépôt, dans l'ordre fixe de DEC-102 :
/// `.nephila` d'abord, les autres seulement si c'est un dossier réel.
const GUARDED_PATHS: [&str; 6] = [
    ".nephila",
    ".nephila/graph.db",
    ".nephila/lock",
    ".nephila/graph.db-wal",
    ".nephila/graph.db-shm",
    ".nephila/graph.db-journal",
];

/// Refuse un `.nephila` ou un de ses fichiers qui est un lien symbolique (REQ-098, E-23), sans
/// suivre aucun lien. Un chemin absent n'est pas une erreur. À appeler avant toute création ou
/// ouverture du verrou et de la base.
pub fn ensure_no_symlink(repo: &Path) -> Result<()> {
    let is_link = |rel: &str| {
        std::fs::symlink_metadata(repo.join(rel))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    };
    let fail = |rel: &str| {
        Err(Error::Message(format!(
            "{rel} est un lien symbolique ; supprimez-le"
        )))
    };
    if is_link(GUARDED_PATHS[0]) {
        return fail(GUARDED_PATHS[0]);
    }
    let real_dir = std::fs::symlink_metadata(repo.join(GUARDED_PATHS[0]))
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false);
    if !real_dir {
        return Ok(());
    }
    match GUARDED_PATHS[1..].iter().find(|rel| is_link(rel)) {
        Some(rel) => fail(rel),
        None => Ok(()),
    }
}

pub struct Store {
    conn: Connection,
    read_only: bool,
    /// Version du schéma lue à l'ouverture (5 possible en lecture seule, DEC-066).
    version: i32,
    /// Base fraîche (DEC-075) : posé par `reset_with_pending_files`, tombe à la première écriture
    /// qui n'est pas une écriture de structure.
    fresh: bool,
    /// Fichiers dont la structure a été écrite depuis le vidage (base fraîche seulement).
    written: std::collections::HashSet<String>,
    /// `meta.resolution` vaut déjà `à résoudre` (DEC-076) : la première écriture après une passe
    /// ou l'ouverture l'écrit, les suivantes non.
    stale: bool,
    /// Journal des changements écrits depuis la dernière passe terminée (DEC-077) : `None` quand
    /// le graphe n'est pas marqué `à jour`.
    log: Option<ChangeLog>,
}

/// Ce que le stockage retient des écritures faites depuis la dernière passe terminée (DEC-077,
/// point 1). Les ensembles se cumulent d'une transaction validée à l'autre, sans annulation
/// mutuelle (un nœud inséré puis supprimé figure dans les deux : la passe lit plus, jamais moins).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ChangeLog {
    /// Identifiants insérés dans `nodes` (dépendances dérivées créées par une passe comprises).
    pub(crate) inserted: std::collections::BTreeSet<String>,
    /// Identifiants supprimés de `nodes` (dépendances dérivées supprimées par une passe comprises).
    pub(crate) deleted: std::collections::BTreeSet<String>,
    /// Identifiants dont l'ensemble des déclarations a changé : différence nette par transaction
    /// de fichier entre les nœuds déclarés avant et après (un résumé seul ne compte pas ; un nœud
    /// inséré ou supprimé y figure aussi).
    pub(crate) declarations_changed: std::collections::BTreeSet<String>,
    /// Fichiers dont des références ont été écrites ou retirées.
    pub(crate) files: std::collections::BTreeSet<String>,
    /// Sources distinctes des références retirées.
    pub(crate) removed_sources: std::collections::BTreeSet<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ChangeLog {
    pub(crate) fn is_empty(&self) -> bool {
        *self == ChangeLog::default()
    }

    fn merge(&mut self, other: ChangeLog) {
        self.inserted.extend(other.inserted);
        self.deleted.extend(other.deleted);
        self.declarations_changed.extend(other.declarations_changed);
        self.files.extend(other.files);
        self.removed_sources.extend(other.removed_sources);
    }
}

/// État d'une transaction de fichier : identifiants touchés (à projeter, DEC-075) et, si le
/// journal est tenu (`track`), les changements qu'elle apporte.
struct Touched {
    ids: std::collections::BTreeSet<String>,
    track: bool,
    /// Nœuds déclarés par le fichier avant la première libération (journal seulement).
    declared_before: Option<std::collections::BTreeSet<String>>,
    log: ChangeLog,
}

impl Touched {
    fn new(track: bool) -> Touched {
        Touched {
            ids: std::collections::BTreeSet::new(),
            track,
            declared_before: None,
            log: ChangeLog::default(),
        }
    }

    fn insert(&mut self, id: String) {
        self.ids.insert(id);
    }

    fn extend<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.ids.extend(ids);
    }
}

impl Store {
    fn new(conn: Connection, read_only: bool) -> Store {
        Store {
            conn,
            read_only,
            version: SCHEMA_VERSION,
            fresh: false,
            written: std::collections::HashSet::new(),
            stale: true,
            log: None,
        }
    }

    /// Lit `meta.resolution` à l'ouverture en écriture : tout autre état que `à jour` compte
    /// comme « à résoudre ».
    fn load_resolution_marker(&mut self) -> Result<()> {
        let value = self.read_resolution_marker()?;
        self.stale = value.as_deref() != Some(RESOLUTION_DONE);
        self.log = (!self.stale).then(ChangeLog::default);
        Ok(())
    }

    /// Journal des changements depuis la dernière passe terminée ; `None` si le graphe n'est pas
    /// marqué `à jour` (ouverture d'un graphe `à résoudre`, vidage d'un `build`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn change_log(&self) -> Option<&ChangeLog> {
        self.log.as_ref()
    }

    fn merge_log(&mut self, log: ChangeLog) {
        if let Some(l) = &mut self.log {
            l.merge(log);
        }
    }

    fn end_fresh(&mut self) {
        self.fresh = false;
        self.written.clear();
    }

    /// Ouvre le graphe en écriture, en le créant (dossier `.nephila/`, schéma, WAL) si besoin.
    pub fn open_or_create(repo: &Path) -> Result<Store> {
        ensure_no_symlink(repo)?;
        let path = db_path(repo);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                Error::Message(format!("impossible de créer {} : {e}", dir.display()))
            })?;
        }
        let conn = Connection::open(&path).map_err(db_err)?;
        // Avant le passage en WAL, qui peut lui-même rencontrer un verrou.
        conn.busy_timeout(BUSY_TIMEOUT).map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        configure_write(&conn)?;
        let mut store = Store::new(conn, false);
        match store.user_version()? {
            0 => store.create_schema()?,
            v if v == SCHEMA_VERSION => {}
            SCHEMA_WITHOUT_REV | SCHEMA_WITH_REF_CONTEXT | SCHEMA_WITH_FILE_CONTEXTS => {
                store.migrate_to_current()?
            }
            // Schéma plus ancien : build repart de zéro, on le recrée.
            v if v > 0 && v < SCHEMA_VERSION => store.recreate_schema()?,
            v => return Err(version_error(v)),
        }
        store.load_resolution_marker()?;
        Ok(store)
    }

    /// Ouvre en écriture un graphe existant, sans rien créer ni migrer : un schéma d'une autre
    /// version (ou une base vide) est refusé avec l'invitation à lancer `nephila build`.
    pub fn open_existing(repo: &Path) -> Result<Store> {
        ensure_no_symlink(repo)?;
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
        configure_write(&conn)?;
        let mut store = Store::new(conn, false);
        match store.user_version()? {
            v if v == SCHEMA_VERSION => {
                store.load_resolution_marker()?;
                Ok(store)
            }
            SCHEMA_WITHOUT_REV | SCHEMA_WITH_REF_CONTEXT | SCHEMA_WITH_FILE_CONTEXTS => {
                store.migrate_to_current()?;
                store.load_resolution_marker()?;
                Ok(store)
            }
            v => Err(Error::Message(format!(
                "graphe de schéma {v} incompatible (attendu {SCHEMA_VERSION}) dans {} : lancez `nephila build`",
                path.display()
            ))),
        }
    }

    /// Ouvre le graphe en lecture seule, sans rien créer, dans une transaction de lecture qui
    /// dure autant que le `Store` (REQ-085).
    pub fn open_readonly(repo: &Path) -> Result<Store> {
        ensure_no_symlink(repo)?;
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
        conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
        // Instantané de lecture pour toute la vie du `Store` (DEC-094) : la première lecture le
        // fixe, avant celle de la version du schéma ; la fermeture de la connexion y met fin.
        conn.execute_batch("BEGIN DEFERRED").map_err(db_err)?;
        conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(db_err)?;
        let mut store = Store::new(conn, true);
        match store.user_version()? {
            0 => {
                return Err(Error::Message(format!(
                    "graphe vide ou illisible dans {} : lancez `nephila build`",
                    path.display()
                )))
            }
            v if v == SCHEMA_VERSION
                || v == SCHEMA_WITHOUT_REV
                || v == SCHEMA_WITH_REF_CONTEXT
                || v == SCHEMA_WITH_FILE_CONTEXTS =>
            {
                store.version = v
            }
            v => return Err(version_error(v)),
        }
        Ok(store)
    }

    /// Vrai pour un `Store` en lecture seule dont la transaction de lecture ouverte par
    /// `open_readonly` est toujours en cours (DEC-094).
    pub fn in_read_snapshot(&self) -> bool {
        self.read_only && !self.conn.is_autocommit()
    }

    /// Version du schéma du graphe ouvert (5 à 7 en lecture seule seulement : DEC-066, DEC-073,
    /// DEC-076).
    pub fn opened_schema_version(&self) -> i32 {
        self.version
    }

    /// Migre un schéma 5, 6 ou 7 en schéma 8 sur place : 5 -> 6, 6 -> 7, puis 7 -> 8, chacune en
    /// une seule transaction. Rien n'est perdu.
    fn migrate_to_current(&self) -> Result<()> {
        if self.user_version()? == SCHEMA_WITHOUT_REV {
            self.migrate_5_to_6()?;
        }
        self.migrate_6_to_7()?;
        self.migrate_7_to_8()
    }

    /// Migre un schéma 7 en schéma 8 en une transaction `IMMEDIATE` (DEC-076) : colonnes et index
    /// de `refs`, clé et tête de chaque référence existante (calculées par nephila), marqueur
    /// `à résoudre`, versions. Sans `IF NOT EXISTS` : un objet en conflit fait échouer la
    /// migration, qui est alors annulée en entier.
    fn migrate_7_to_8(&self) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(db_err)?;
        // Un autre processus a pu migrer entre la lecture de la version et le verrou.
        if self.user_version()? != SCHEMA_WITH_FILE_CONTEXTS {
            return Ok(());
        }
        tx.execute_batch(
            "ALTER TABLE refs ADD COLUMN target_key TEXT NOT NULL DEFAULT '';
             ALTER TABLE refs ADD COLUMN target_head TEXT;
             ALTER TABLE refs ADD COLUMN unresolved INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(db_err)?;
        let rows: Vec<(i64, String)> = {
            let mut st = tx
                .prepare_cached("SELECT id, target_name FROM refs")
                .map_err(db_err)?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db_err)?;
            rows.collect::<std::result::Result<_, _>>()
                .map_err(db_err)?
        };
        {
            let mut up = tx
                .prepare_cached("UPDATE refs SET target_key = ?2, target_head = ?3 WHERE id = ?1")
                .map_err(db_err)?;
            for (id, name) in rows {
                let (key, head) = crate::resolve::name_keys(&name);
                up.execute(params![id, key, head]).map_err(db_err)?;
            }
        }
        tx.execute_batch(&format!(
            "CREATE INDEX idx_refs_key ON refs(target_key);
             CREATE INDEX idx_refs_head ON refs(target_head) WHERE target_head IS NOT NULL;
             CREATE INDEX idx_refs_unresolved ON refs(unresolved) WHERE unresolved = 1;
             INSERT OR REPLACE INTO meta(key, value) VALUES ('resolution', '{RESOLUTION_PENDING}');
             UPDATE meta SET value = '{SCHEMA_VERSION}' WHERE key = 'schema_version';
             PRAGMA user_version = {SCHEMA_VERSION};"
        ))
        .map_err(db_err)?;
        commit_counted(tx)
    }

    /// Migre un schéma 6 en schéma 7 en une transaction `IMMEDIATE` (DEC-073) : `refs` reconstruite
    /// avec les mêmes identifiants dans le même ordre, contextes reportés dans `file_contexts`,
    /// index supprimés et créés, versions. Sans `IF NOT EXISTS` : un objet en conflit fait
    /// échouer la migration, qui est alors annulée en entier.
    fn migrate_6_to_7(&self) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(db_err)?;
        // Un autre processus a pu migrer entre la lecture de la version et le verrou.
        if self.user_version()? != SCHEMA_WITH_REF_CONTEXT {
            return Ok(());
        }
        tx.execute_batch(&format!(
            "CREATE TABLE file_contexts (
                 file    TEXT PRIMARY KEY REFERENCES files(path) ON DELETE CASCADE,
                 context TEXT NOT NULL
             );
             INSERT INTO file_contexts(file, context)
                 SELECT file, context FROM (
                     SELECT file, context, min(id) FROM refs
                     WHERE origin = 'parse' AND context IS NOT NULL GROUP BY file);
             CREATE TABLE refs_v7 (
                 id          INTEGER PRIMARY KEY,
                 source      TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                 file        TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
                 kind        TEXT NOT NULL,
                 target_name TEXT NOT NULL,
                 origin      TEXT NOT NULL DEFAULT 'parse' CHECK (origin IN ('parse', 'llm'))
             );
             INSERT INTO refs_v7(id, source, file, kind, target_name, origin)
                 SELECT id, source, file, kind, target_name, origin FROM refs ORDER BY id;
             DROP TABLE refs;
             ALTER TABLE refs_v7 RENAME TO refs;
             CREATE INDEX idx_refs_source ON refs(source);
             CREATE INDEX idx_refs_file ON refs(file);
             DELETE FROM sqlite_sequence WHERE name = 'refs';
             DROP INDEX idx_edges_source;
             DROP INDEX idx_declarations_node;
             CREATE INDEX idx_declarations_node_file ON declarations(node, file);
             UPDATE meta SET value = '{SCHEMA_WITH_FILE_CONTEXTS}' WHERE key = 'schema_version';
             PRAGMA user_version = {SCHEMA_WITH_FILE_CONTEXTS};"
        ))
        .map_err(db_err)?;
        commit_counted(tx)
    }

    /// Migre un schéma 5 en schéma 6 sur place, en une seule transaction : colonne, remplissage
    /// (inverse calculé en Rust), index, `meta.schema_version`, `user_version`. Rien n'est perdu.
    fn migrate_5_to_6(&self) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(db_err)?;
        // Un autre processus a pu migrer entre la lecture de la version et le verrou.
        if self.user_version()? != SCHEMA_WITHOUT_REV {
            return Ok(());
        }
        tx.execute_batch("ALTER TABLE nodes ADD COLUMN qualified_rev TEXT NOT NULL DEFAULT ''")
            .map_err(db_err)?;
        let rows: Vec<(String, String)> = {
            let mut st = tx
                .prepare_cached("SELECT id, qualified FROM nodes")
                .map_err(db_err)?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db_err)?;
            rows.collect::<std::result::Result<_, _>>()
                .map_err(db_err)?
        };
        {
            let mut up = tx
                .prepare_cached("UPDATE nodes SET qualified_rev = ?2 WHERE id = ?1")
                .map_err(db_err)?;
            for (id, q) in rows {
                up.execute(params![id, reverse_qualified(&q)])
                    .map_err(db_err)?;
            }
        }
        tx.execute_batch(&format!(
            "CREATE INDEX idx_nodes_qualified_rev ON nodes(qualified_rev);
             UPDATE meta SET value = '{SCHEMA_WITH_REF_CONTEXT}' WHERE key = 'schema_version';
             PRAGMA user_version = {SCHEMA_WITH_REF_CONTEXT};"
        ))
        .map_err(db_err)?;
        commit_counted(tx)
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
                .prepare_cached("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
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
            "BEGIN;\n{SCHEMA}\nINSERT INTO meta(key, value) VALUES ('schema_version', '{SCHEMA_VERSION}'), ('resolution', '{RESOLUTION_PENDING}');\nPRAGMA user_version = {SCHEMA_VERSION};\nCOMMIT;"
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
        self.end_fresh();
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute_batch(
            "DELETE FROM edges; DELETE FROM file_contexts; DELETE FROM refs; DELETE FROM declarations; DELETE FROM nodes; DELETE FROM files;
             DELETE FROM summary; DELETE FROM meta WHERE key NOT IN ('schema_version', 'enrich_model', 'enrich_code');",
        )
        .map_err(db_err)?;
        set_resolution_tx(&tx, RESOLUTION_PENDING)?;
        commit_counted(tx)?;
        self.stale = true;
        self.log = None;
        Ok(())
    }

    /// Vide le graphe et enregistre `files` « en attente » en une seule transaction : le graphe
    /// précédent n'est remplacé que si l'ensemble réussit.
    pub fn reset_with_pending_files(&mut self, files: &[FileRecord]) -> Result<()> {
        self.ensure_writable()?;
        self.end_fresh();
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute_batch(
            "DELETE FROM edges; DELETE FROM file_contexts; DELETE FROM refs; DELETE FROM declarations; DELETE FROM nodes; DELETE FROM files;
             DELETE FROM summary; DELETE FROM meta WHERE key NOT IN ('schema_version', 'enrich_model', 'enrich_code');",
        )
        .map_err(db_err)?;
        set_resolution_tx(&tx, RESOLUTION_PENDING)?;
        {
            let mut st = tx
                .prepare_cached("INSERT INTO files(path, hash, size, mtime, recorded_at, state) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')")
                .map_err(db_err)?;
            for f in files {
                let size = i64::try_from(f.size)
                    .map_err(|_| Error::Message(format!("taille invalide pour {}", f.path)))?;
                st.execute(params![f.path, f.hash, size, f.mtime, now_ms()])
                    .map_err(db_err)?;
            }
        }
        commit_counted(tx)?;
        self.stale = true;
        self.log = None;
        self.fresh = true;
        Ok(())
    }

    /// Enregistre ou met à jour des fichiers (état d'origine conservé dans chaque enregistrement),
    /// en une seule transaction. Un fichier déjà connu garde ses nœuds.
    pub fn insert_files(&mut self, files: &[FileRecord]) -> Result<()> {
        self.ensure_writable()?;
        self.end_fresh();
        let tx = self.conn.transaction().map_err(db_err)?;
        {
            let mut st = tx
                .prepare_cached(
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
        commit_counted(tx)
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
    /// origines `parse` et `llm`), efface les résumés de ses nœuds, puis marque le fichier indexé.
    /// L'enrichissement conservé de même empreinte de contenu est réappliqué dans la même
    /// transaction, un enrichissement de contenu différent est supprimé (DEC-062). Le fichier doit être connu. Un nœud peut être déclaré
    /// par plusieurs fichiers (DEC-014) : la ligne du nœud (attributs, fichier de rattachement)
    /// est une projection des déclarations (DEC-026 : attributs et fichier de la déclaration de plus petit chemin, résumé de la plus petite déclaration résumée), les références restent attachées au fichier qui les a
    /// émises. Un nœud sans fichier (concept) est déclaré lui aussi par le fichier
    /// qui le mentionne et disparaît avec son dernier déclarant. Les nœuds `dependency` sont
    /// dérivés par la résolution (DEC-020) : ils ne sont jamais supprimés ici, seul
    /// `replace_derived_dependencies` les nettoie. Les références dont la source
    /// n'est pas déclarée par le fichier sont rejetées. Déclarations et références nouvelles
    /// ont l'origine `parse`.
    pub fn replace_structure(&mut self, path: &str, nodes: &[Node], refs: &[Ref]) -> Result<()> {
        let one = [(path.to_string(), nodes.to_vec(), refs.to_vec())];
        self.replace_structures(&one)
    }

    /// Comme `replace_structure` pour plusieurs fichiers en une seule transaction (tout ou rien) :
    /// évite une validation par fichier lors d'un build de milliers de fichiers.
    pub fn replace_structures(&mut self, items: &[(String, Vec<Node>, Vec<Ref>)]) -> Result<()> {
        self.replace_structures_counted(items).map(|_| ())
    }

    /// Comme `replace_structures` ; renvoie le nombre de fichiers dont l'enrichissement conservé
    /// a été réappliqué (DEC-062). Prépare (DEC-086) puis écrit ; une erreur de préparation est
    /// rendue à son rang, après le contrôle « fichier connu » des fichiers qui la précèdent.
    pub fn replace_structures_counted(
        &mut self,
        items: &[(String, Vec<Node>, Vec<Ref>)],
    ) -> Result<usize> {
        let prepared: Vec<(&str, Result<PreparedStructure>)> = items
            .iter()
            .map(|(p, n, r)| (p.as_str(), prepare_raw(p, n, r)))
            .collect();
        self.write_items(prepared)
    }

    /// Écrit des structures préparées (DEC-086) en une seule transaction, tout ou rien : que des
    /// instructions SQL, aucun calcul. Renvoie le nombre de fichiers dont l'enrichissement
    /// conservé a été réappliqué (DEC-062).
    pub fn write_prepared(&mut self, items: &[PreparedStructure]) -> Result<usize> {
        self.write_items(items.iter().map(|p| (p.path.as_str(), Ok(p))))
    }

    fn write_items<'a, B: Borrow<PreparedStructure>>(
        &mut self,
        items: impl IntoIterator<Item = (&'a str, Result<B>)>,
    ) -> Result<usize> {
        self.ensure_writable()?;
        let tx = self.conn.transaction().map_err(db_err)?;
        let mut reapplied = 0;
        let mut newly: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut any = false;
        let track = self.log.is_some();
        let mut pending = ChangeLog::default();
        for (path, item) in items {
            if !any {
                mark_stale_tx(self.stale, &tx)?;
                any = true;
            }
            require_known(&tx, path)?;
            let p = item?;
            let p: &PreparedStructure = p.borrow();
            let fresh = self.fresh && !self.written.contains(path) && !newly.contains(path);
            if fresh {
                if write_structure_fresh(&tx, p)? {
                    reapplied += 1;
                }
                newly.insert(path);
                continue;
            }
            let mut touched = Touched::new(track);
            clear_summaries(&tx, path, &mut touched)?;
            release_file(&tx, path, None, &mut touched)?;
            insert_nodes(&tx, path, &p.nodes, "parse", &mut touched)?;
            insert_prepared_refs(&tx, p, &mut touched)?;
            tx.prepare_cached("UPDATE files SET state = 'indexed' WHERE path = ?1")
                .map_err(db_err)?
                .execute([path])
                .map_err(db_err)?;
            if reapply_enrichment(&tx, path, &mut touched)? {
                reapplied += 1;
            }
            finish_file(&tx, path, &mut touched)?;
            pending.merge(touched.log);
        }
        commit_counted(tx)?;
        self.merge_log(pending);
        if any {
            self.stale = true;
        }
        if self.fresh {
            let done: Vec<String> = newly.iter().map(|p| (*p).to_string()).collect();
            self.written.extend(done);
        }
        Ok(reapplied)
    }

    /// Applique en une transaction le résultat de l'enrichissement d'un fichier : remplace ses
    /// déclarations et références d'origine `llm` (nœuds concept compris) et les résumés de ses
    /// nœuds, puis enregistre l'enrichissement conservé du chemin (`hash` = empreinte du contenu,
    /// `model`, `ENRICH_PROMPT_VERSION`, payload). `summaries` : couples (identifiant, texte), les
    /// identifiants devant être déclarés par le fichier. Les références `llm` doivent partir d'un
    /// nœud déclaré par le fichier.
    pub fn apply_enrichment(
        &mut self,
        path: &str,
        hash: &str,
        model: &str,
        summaries: &[(String, String)],
        concept_nodes: &[Node],
        refs: &[Ref],
    ) -> Result<()> {
        self.apply_enrichments(
            model,
            &[EnrichmentWrite {
                path: path.to_string(),
                hash: hash.to_string(),
                summaries: summaries.to_vec(),
                concepts: concept_nodes.to_vec(),
                refs: refs.to_vec(),
            }],
        )
    }

    /// Comme `apply_enrichment` pour plusieurs fichiers en une seule transaction, tout ou rien
    /// (DEC-074) : mêmes contrôles et même entrée d'enrichissement conservé par fichier.
    pub fn apply_enrichments(&mut self, model: &str, items: &[EnrichmentWrite]) -> Result<()> {
        self.ensure_writable()?;
        self.end_fresh();
        if items.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction().map_err(db_err)?;
        mark_stale_tx(self.stale, &tx)?;
        let track = self.log.is_some();
        let mut pending = ChangeLog::default();
        for item in items {
            pending.merge(write_enrichment(&tx, model, item, track)?);
        }
        commit_counted(tx)?;
        self.merge_log(pending);
        self.stale = true;
        Ok(())
    }

    /// Fichiers indexés à enrichir : dans la portée de l'enrichissement (documents, plus les
    /// fichiers de code si `meta.enrich_code`, REQ-050) et sans enrichissement conservé pour
    /// (chemin, empreinte, modèle effectif, `ENRICH_PROMPT_VERSION`). Le modèle effectif est celui
    /// de `meta.enrich_model` (écrit par `build`/`update` LLM actif) ; absent, le modèle n'est
    /// pas comparé.
    pub fn files_to_enrich(&self) -> Result<Vec<FileRecord>> {
        let model = self.enrich_model()?;
        let code = self.enrich_code()?;
        let mut st = self
            .conn
            .prepare_cached(&format!(
                "SELECT f.path FROM files f WHERE f.state = 'indexed' AND NOT EXISTS ({UP_TO_DATE}) ORDER BY f.path"
            ))
            .map_err(db_err)?;
        let paths: Vec<String> = st
            .query_map(params![ENRICH_PROMPT_VERSION, model], |r| r.get(0))
            .map_err(db_err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        let set: std::collections::HashSet<String> = paths
            .into_iter()
            .filter(|p| code || crate::llm::enrich::is_markdown(p))
            .collect();
        Ok(self
            .files()?
            .into_iter()
            .filter(|f| set.contains(&f.path))
            .collect())
    }

    /// Nombre de fichiers à enrichir.
    pub fn count_files_to_enrich(&self) -> Result<usize> {
        Ok(self.files_to_enrich()?.len())
    }

    /// Portée enregistrée par le dernier `build`/`update` LLM actif (`false` : documents seuls).
    pub fn enrich_code(&self) -> Result<bool> {
        match self
            .conn
            .prepare_cached("SELECT value FROM meta WHERE key = 'enrich_code'")
            .map_err(db_err)?
            .query_row([], |r| r.get::<_, String>(0))
        {
            Ok(v) => Ok(v == "true"),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Enregistre la portée effective d'une commande LLM active.
    pub fn set_enrich_code(&mut self, code: bool) -> Result<()> {
        self.ensure_writable()?;
        self.conn
            .prepare_cached(
                "INSERT INTO meta(key, value) VALUES ('enrich_code', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .map_err(db_err)?
            .execute([if code { "true" } else { "false" }])
            .map_err(db_err)?;
        Ok(())
    }

    /// Modèle effectif du dernier `build`/`update` LLM actif (`None` : aucun depuis la
    /// réinitialisation).
    pub fn enrich_model(&self) -> Result<Option<String>> {
        match self
            .conn
            .prepare_cached("SELECT value FROM meta WHERE key = 'enrich_model'")
            .map_err(db_err)?
            .query_row([], |r| r.get::<_, String>(0))
        {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Enregistre le modèle effectif d'une commande LLM active.
    pub fn set_enrich_model(&mut self, model: &str) -> Result<()> {
        self.ensure_writable()?;
        self.conn
            .prepare_cached(
                "INSERT INTO meta(key, value) VALUES ('enrich_model', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .map_err(db_err)?
            .execute([model])
            .map_err(db_err)?;
        Ok(())
    }

    /// Empreinte de contenu de l'enrichissement conservé d'un chemin (`None` : aucune entrée).
    pub fn enriched_hash(&self, path: &str) -> Result<Option<String>> {
        Ok(self.enrichment_entry(path)?.map(|e| e.content_hash))
    }

    /// Entrée d'enrichissement conservée d'un chemin, sans son contenu.
    pub fn enrichment_entry(&self, path: &str) -> Result<Option<EnrichmentEntry>> {
        match self
            .conn
            .prepare_cached(
                "SELECT content_hash, model, prompt_version FROM enrichments WHERE path = ?1",
            )
            .map_err(db_err)?
            .query_row([path], |r| {
                Ok(EnrichmentEntry {
                    content_hash: r.get(0)?,
                    model: r.get(1)?,
                    prompt_version: r.get(2)?,
                })
            }) {
            Ok(e) => Ok(Some(e)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Supprime les enrichissements conservés d'un chemin qui n'est plus connu du graphe ou dont
    /// le contenu a changé. Renvoie le nombre d'entrées supprimées.
    pub fn prune_enrichments(&mut self) -> Result<usize> {
        self.ensure_writable()?;
        self.end_fresh();
        self.conn
            .prepare_cached(
                "DELETE FROM enrichments WHERE NOT EXISTS
                   (SELECT 1 FROM files f WHERE f.path = enrichments.path
                    AND f.hash = enrichments.content_hash)",
            )
            .map_err(db_err)?
            .execute([])
            .map_err(db_err)
    }

    /// Supprime un fichier, ses nœuds, références et les liens attachés. Renvoie `false` si inconnu.
    pub fn remove_file(&mut self, path: &str) -> Result<bool> {
        Ok(self.remove_files(&[path])? > 0)
    }

    /// Supprime plusieurs fichiers (même effet que `remove_file` pour chacun) en une seule
    /// transaction, tout ou rien (DEC-074). Renvoie le nombre de fichiers qui existaient.
    pub fn remove_files(&mut self, paths: &[&str]) -> Result<usize> {
        self.ensure_writable()?;
        self.end_fresh();
        if paths.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction().map_err(db_err)?;
        mark_stale_tx(self.stale, &tx)?;
        let mut removed = 0;
        let track = self.log.is_some();
        let mut pending = ChangeLog::default();
        for path in paths {
            let mut touched = Touched::new(track);
            clear_summaries(&tx, path, &mut touched)?;
            release_file(&tx, path, None, &mut touched)?;
            finish_file(&tx, path, &mut touched)?;
            pending.merge(touched.log);
            tx.prepare_cached("DELETE FROM enrichments WHERE path = ?1")
                .map_err(db_err)?
                .execute([path])
                .map_err(db_err)?;
            removed += tx
                .prepare_cached("DELETE FROM files WHERE path = ?1")
                .map_err(db_err)?
                .execute([path])
                .map_err(db_err)?;
        }
        commit_counted(tx)?;
        self.merge_log(pending);
        self.stale = true;
        Ok(removed)
    }

    /// Remplace l'ensemble des liens dérivés (résolution, REQ-008).
    pub fn replace_edges(&mut self, edges: &[Edge]) -> Result<()> {
        self.ensure_writable()?;
        self.end_fresh();
        let tx = self.conn.transaction().map_err(db_err)?;
        replace_edges_tx(&tx, edges)?;
        commit_counted(tx)
    }

    pub fn nodes(&self) -> Result<Vec<Node>> {
        let mut st = self
            .conn
            .prepare_cached(
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
        note_node_rows(out.len());
        Ok(out)
    }

    /// Parcourt les nœuds dans l'ordre de `nodes()`, un à la fois (DEC-092) : la mémoire ne
    /// dépend pas de la taille du graphe. Une erreur de `f` arrête le parcours et est rendue.
    pub fn for_each_node<E, F>(&self, mut f: F) -> std::result::Result<(), E>
    where
        E: From<Error>,
        F: FnMut(&Node) -> std::result::Result<(), E>,
    {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT id, kind, qualified, short, file, subkind, summary FROM nodes ORDER BY id",
            )
            .map_err(db_err)?;
        let mut rows = st.query([]).map_err(db_err)?;
        while let Some(r) = rows.next().map_err(db_err)? {
            let kind: String = r.get(1).map_err(db_err)?;
            let subkind: Option<String> = r.get(5).map_err(db_err)?;
            let node = Node {
                id: r.get(0).map_err(db_err)?,
                kind: NodeKind::parse(&kind)?,
                qualified: r.get(2).map_err(db_err)?,
                short: r.get(3).map_err(db_err)?,
                file: r.get(4).map_err(db_err)?,
                subkind: subkind.as_deref().map(SubKind::parse).transpose()?,
                summary: r.get(6).map_err(db_err)?,
            };
            f(&node)?;
        }
        Ok(())
    }

    /// Nœuds susceptibles de correspondre à un nom saisi : identifiant, nom qualifié ou nom court
    /// égal à `name`, ou nom qualifié terminé par `name`. Triés par identifiant. L'appelant
    /// affine (frontière de segment).
    ///
    /// Schéma 6 : requêtes séparées sur index, réunies ici (DEC-066). Schéma 5 (lecture seule) :
    /// requête de la 0.18.0.
    pub fn nodes_matching(&self, name: &str) -> Result<Vec<Node>> {
        if self.version < SCHEMA_WITH_FILE_CONTEXTS {
            return self.run_resolve(RESOLVE_LEGACY, [name].as_slice());
        }
        let mut found = self.run_resolve(RESOLVE_ID, [name].as_slice())?;
        found.extend(self.run_resolve(RESOLVE_QUALIFIED, [name].as_slice())?);
        found.extend(self.run_resolve(RESOLVE_SHORT, [name].as_slice())?);
        let rev = reverse_qualified(name);
        for (c, next) in RESOLVE_BOUNDARIES {
            let lo = format!("{rev}{c}");
            let hi = format!("{rev}{next}");
            found.extend(self.run_resolve(RESOLVE_SUFFIX, [lo.as_str(), hi.as_str()].as_slice())?);
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        found.dedup_by(|a, b| a.id == b.id);
        Ok(found)
    }

    pub(crate) fn run_resolve(&self, sql: &str, params: &[&str]) -> Result<Vec<Node>> {
        let mut st = self.conn.prepare_cached(sql).map_err(db_err)?;
        let rows = st
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
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
        note_node_rows(out.len());
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
            Ok((id, kind, qualified, short, file, subkind, summary)) => {
                note_node_rows(1);
                Ok(Some(Node {
                    id,
                    kind: NodeKind::parse(&kind)?,
                    qualified,
                    short,
                    file,
                    subkind: subkind.as_deref().map(SubKind::parse).transpose()?,
                    summary,
                }))
            }
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
        note_edge_rows(out.len());
        Ok(out)
    }

    pub fn count_edges(&self) -> Result<usize> {
        self.conn
            .prepare_cached(COUNT_EDGES)
            .map_err(db_err)?
            .query_row([], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .map_err(db_err)
    }

    pub fn count_nodes(&self) -> Result<usize> {
        self.count("nodes")
    }

    fn count(&self, table: &str) -> Result<usize> {
        self.conn
            .prepare_cached(&format!("SELECT COUNT(*) FROM {table}"))
            .map_err(db_err)?
            .query_row([], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .map_err(db_err)
    }

    pub fn edges(&self) -> Result<Vec<Edge>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT source, target, kind FROM edges ORDER BY source, target, kind")
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
        note_edge_rows(out.len());
        Ok(out)
    }

    /// Parcourt les liens dans l'ordre de `edges()`, un à la fois (DEC-092).
    pub fn for_each_edge<E, F>(&self, mut f: F) -> std::result::Result<(), E>
    where
        E: From<Error>,
        F: FnMut(&Edge) -> std::result::Result<(), E>,
    {
        let mut st = self
            .conn
            .prepare_cached("SELECT source, target, kind FROM edges ORDER BY source, target, kind")
            .map_err(db_err)?;
        let mut rows = st.query([]).map_err(db_err)?;
        while let Some(r) = rows.next().map_err(db_err)? {
            let kind: String = r.get(2).map_err(db_err)?;
            let edge = Edge {
                source: r.get(0).map_err(db_err)?,
                target: r.get(1).map_err(db_err)?,
                kind: EdgeKind::parse(&kind)?,
            };
            f(&edge)?;
        }
        Ok(())
    }

    /// Valeur de `meta.resolution`, `None` si la clé manque (schémas 5 à 7). Lecture seule.
    fn read_resolution_marker(&self) -> Result<Option<String>> {
        match self
            .conn
            .prepare_cached("SELECT value FROM meta WHERE key = 'resolution'")
            .map_err(db_err)?
            .query_row([], |r| r.get(0))
        {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Vrai si le graphe est de schéma 8 et marqué `à jour` (`meta.resolution`) : le statut
    /// `refs.unresolved` est alors celui d'une résolution terminée. Lecture seule (DEC-092).
    pub fn resolution_is_current(&self) -> Result<bool> {
        if self.version != SCHEMA_VERSION {
            return Ok(false);
        }
        Ok(self.read_resolution_marker()?.as_deref() == Some(RESOLUTION_DONE))
    }

    /// Vrai si le graphe est « à résoudre » : `meta.resolution` différent de `à jour`, ou clé
    /// absente (REQ-088, DEC-097). Lecture seule, sans effet sur `graph.db`.
    pub fn needs_resolution(&self) -> Result<bool> {
        Ok(self.read_resolution_marker()?.as_deref() != Some(RESOLUTION_DONE))
    }

    /// Parcourt les références comptées non résolues (`refs.unresolved = 1`), dans l'ordre de
    /// `id`, sans copie de la table : `(source, genre, nom cible)`. À n'appeler que si
    /// `resolution_is_current()` (schéma 8).
    pub fn for_each_unresolved<E, F>(&self, mut f: F) -> std::result::Result<(), E>
    where
        E: From<Error>,
        F: FnMut(&str, EdgeKind, &str) -> std::result::Result<(), E>,
    {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT source, kind, target_name FROM refs WHERE unresolved = 1 ORDER BY id",
            )
            .map_err(db_err)?;
        let mut rows = st.query([]).map_err(db_err)?;
        while let Some(r) = rows.next().map_err(db_err)? {
            let kind: String = r.get(1).map_err(db_err)?;
            let kind = EdgeKind::parse(&kind)?;
            let source = r
                .get_ref(0)
                .map_err(db_err)?
                .as_str()
                .map_err(|e| db_err(e.into()))?;
            let target = r
                .get_ref(2)
                .map_err(db_err)?
                .as_str()
                .map_err(|e| db_err(e.into()))?;
            f(source, kind, target)?;
        }
        Ok(())
    }

    pub fn refs(&self) -> Result<Vec<Ref>> {
        Ok(self
            .refs_with_origin()?
            .into_iter()
            .map(|(r, _)| r)
            .collect())
    }

    /// Références avec leur fichier et leur origine, dans l'ordre d'insertion, sans contexte.
    pub fn file_refs(&self) -> Result<Vec<FileRef>> {
        let mut st = self.conn.prepare_cached(FILE_REFS_SQL).map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, source, kind, target_name, origin, file) = row.map_err(db_err)?;
            out.push(FileRef {
                id,
                r: Ref {
                    source,
                    kind: EdgeKind::parse(&kind)?,
                    target_name,
                    context: None,
                },
                origin: if origin == "llm" {
                    RefOrigin::Llm
                } else {
                    RefOrigin::Parse
                },
                file,
            });
        }
        Ok(out)
    }

    /// Contexte d'imports de chaque fichier qui en a un (`file_contexts` ; sur un schéma 5 ou 6
    /// en lecture seule, le contexte de ses références d'analyse).
    pub fn file_contexts(&self) -> Result<std::collections::HashMap<String, String>> {
        let sql = if self.version < SCHEMA_WITH_FILE_CONTEXTS {
            FILE_CONTEXTS_LEGACY_SQL
        } else {
            FILE_CONTEXTS_SQL
        };
        let mut st = self.conn.prepare_cached(sql).map_err(db_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Références avec leur origine (`parse` ou `llm`), dans l'ordre d'insertion. Le contexte
    /// d'une référence d'analyse est celui de son fichier (`file_contexts`), une référence `llm`
    /// n'en a pas ; sur un schéma 5 ou 6 (lecture seule), il vient de `refs.context`.
    pub fn refs_with_origin(&self) -> Result<Vec<(Ref, RefOrigin)>> {
        let sql = if self.version < SCHEMA_WITH_FILE_CONTEXTS {
            REFS_LEGACY_SQL
        } else {
            REFS_SQL
        };
        let mut st = self.conn.prepare_cached(sql).map_err(db_err)?;
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
        self.end_fresh();
        let tx = self.conn.transaction().map_err(db_err)?;
        let changes = replace_dependencies_tx(&tx, deps)?;
        commit_counted(tx)?;
        self.merge_log(changes);
        Ok(())
    }

    /// Écrit en une seule transaction les dépendances dérivées, les liens et le résumé global
    /// d'une passe de résolution (DEC-074). `summary` calcule le résumé sur l'état que la
    /// transaction vient d'écrire (texte, horodatage). Une erreur annule toute la passe.
    ///
    /// Passe complète : le statut des références est remis à zéro (par l'index partiel) puis
    /// posé sur `unresolved` (identifiants de `refs`), et le marqueur passe à `à jour` (DEC-076).
    pub fn write_resolution<F>(
        &mut self,
        deps: &[Node],
        edges: &[Edge],
        unresolved: &[i64],
        summary: F,
    ) -> Result<()>
    where
        F: FnOnce(&Store) -> Result<(String, i64)>,
    {
        self.ensure_writable()?;
        self.end_fresh();
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Deferred,
        )
        .map_err(db_err)?;
        let changes = replace_dependencies_tx(&tx, deps)?;
        replace_edges_tx(&tx, edges)?;
        tx.prepare_cached("UPDATE refs SET unresolved = 0 WHERE unresolved = 1")
            .map_err(db_err)?
            .execute([])
            .map_err(db_err)?;
        {
            let mut mark = tx
                .prepare_cached("UPDATE refs SET unresolved = 1 WHERE id = ?1")
                .map_err(db_err)?;
            for id in unresolved {
                mark.execute([id]).map_err(db_err)?;
            }
        }
        // Les lectures du calcul passent par la même connexion : elles voient la transaction.
        let (text, computed_at) = summary(self)?;
        set_summary_tx(&tx, &text, computed_at)?;
        set_resolution_tx(&tx, RESOLUTION_DONE)?;
        commit_counted(tx)?;
        self.stale = false;
        // La passe remet le journal à vide ; ses nœuds `dependency` créés ou supprimés sont ce
        // que la passe suivante doit relire.
        self.log = Some(changes);
        Ok(())
    }

    /// Nœuds de l'index de résolution : identifiant, genre, nom qualifié, nom court, fichier
    /// (chargement léger de la passe incrémentale, sans résumé ni ordre).
    pub(crate) fn resolution_nodes(&self) -> Result<Vec<Node>> {
        let mut st = self.conn.prepare_cached(RESOLVE_NODES).map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, qualified, short, file) = row.map_err(db_err)?;
            out.push(Node {
                id,
                kind: NodeKind::parse(&kind)?,
                qualified,
                short,
                file,
                subkind: None,
                summary: None,
            });
        }
        note_node_rows(out.len());
        Ok(out)
    }

    /// Premier champ texte de chaque ligne de `sql` pour le paramètre `arg`.
    fn strings_of(&self, sql: &str, arg: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(sql).map_err(db_err)?;
        let rows = st
            .query_map([arg], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Sources des références dont la clé (`by_head` faux) ou la tête (`by_head` vrai) vaut `key`.
    pub(crate) fn ref_sources_by_key(&self, key: &str, by_head: bool) -> Result<Vec<String>> {
        self.strings_of(
            if by_head {
                REF_SOURCES_BY_HEAD
            } else {
                REF_SOURCES_BY_KEY
            },
            key,
        )
    }

    /// Sources des références écrites par le fichier `file`.
    pub(crate) fn ref_sources_of_file(&self, file: &str) -> Result<Vec<String>> {
        self.strings_of(REF_SOURCES_BY_FILE, file)
    }

    /// Cibles des liens `depends_on` stockés d'une source qui sont des nœuds `dependency`
    /// (recherche des dépendances qui disparaissent, REQ-087).
    pub(crate) fn stored_dependencies(&self, source: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(STORED_REF_EDGES).map_err(db_err)?;
        let rows = st
            .query_map([source], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        let prefix = format!("{}:", NodeKind::Dependency.as_str());
        let mut seen = 0;
        for row in rows {
            let (target, kind) = row.map_err(db_err)?;
            seen += 1;
            if kind == EdgeKind::DependsOn.as_str() && target.starts_with(&prefix) {
                out.push(target);
            }
        }
        note_edge_rows(seen);
        Ok(out)
    }

    /// Sources des liens `depends_on` stockés qui visent `target`.
    pub(crate) fn depends_on_sources(&self, target: &str) -> Result<Vec<String>> {
        let out = self.strings_of(DEPENDS_ON_SOURCES, target)?;
        note_edge_rows(out.len());
        Ok(out)
    }

    /// Fichiers qui déclarent le nœud `node`.
    pub(crate) fn declaring_files(&self, node: &str) -> Result<Vec<String>> {
        self.strings_of(DECLARING_FILES, node)
    }

    /// Nœuds déclarés par le fichier `file`.
    pub(crate) fn declared_nodes(&self, file: &str) -> Result<Vec<String>> {
        self.strings_of(DECLARED_NODES, file)
    }

    /// Contexte d'imports d'un fichier, s'il en a un.
    pub(crate) fn file_context(&self, file: &str) -> Result<Option<String>> {
        match self
            .conn
            .prepare_cached(FILE_CONTEXT)
            .map_err(db_err)?
            .query_row([file], |r| r.get::<_, String>(0))
        {
            Ok(c) => Ok(Some(c)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Références dont la source est `source`, dans l'ordre d'insertion, avec leur statut stocké
    /// (vrai : comptée non résolue).
    pub(crate) fn refs_of_source(&self, source: &str) -> Result<Vec<(FileRef, bool)>> {
        let mut st = self.conn.prepare_cached(REFS_OF_SOURCE).map_err(db_err)?;
        let rows = st
            .query_map([source], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, target_name, origin, file, unresolved) = row.map_err(db_err)?;
            out.push((
                FileRef {
                    id,
                    r: Ref {
                        source: source.to_string(),
                        kind: EdgeKind::parse(&kind)?,
                        target_name,
                        context: None,
                    },
                    origin: if origin == "llm" {
                        RefOrigin::Llm
                    } else {
                        RefOrigin::Parse
                    },
                    file,
                },
                unresolved != 0,
            ));
        }
        Ok(out)
    }

    /// Écrit en une transaction une passe incrémentale (DEC-077 point 3) : dépendances dérivées
    /// manquantes, différence des liens de chaque source et des liens `contains` de chaque cible
    /// réévaluées, dépendances devenues inutiles, statut des références, résumé et marqueur.
    /// Le journal repart des seuls nœuds `dependency` créés ou supprimés par la passe.
    pub(crate) fn write_incremental<F>(&mut self, w: &IncrementalWrite, summary: F) -> Result<()>
    where
        F: FnOnce(&Store) -> Result<(String, i64)>,
    {
        use std::collections::{BTreeSet, HashSet};
        self.ensure_writable()?;
        self.end_fresh();
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Deferred,
        )
        .map_err(db_err)?;
        let mut log = ChangeLog::default();
        {
            let mut ins = tx.prepare_cached(INSERT_DEPENDENCY).map_err(db_err)?;
            for n in &w.dependencies {
                let added = ins
                    .execute(params![
                        n.id,
                        n.qualified,
                        n.short,
                        reverse_qualified(&n.qualified)
                    ])
                    .map_err(db_err)?;
                if added > 0 {
                    log.inserted.insert(n.id.clone());
                }
            }
        }
        let dependency_prefix = format!("{}:", NodeKind::Dependency.as_str());
        let depends_on = EdgeKind::DependsOn.as_str();
        let mut candidates: BTreeSet<String> = BTreeSet::new();
        for (source, wanted) in &w.sources {
            let stored: Vec<(String, String)> = {
                let mut st = tx.prepare_cached(STORED_REF_EDGES).map_err(db_err)?;
                let rows = st
                    .query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(db_err)?;
                rows.collect::<std::result::Result<_, _>>()
                    .map_err(db_err)?
            };
            note_edge_rows(stored.len());
            let wanted_set: HashSet<(&str, &str)> = wanted
                .iter()
                .map(|e| (e.target.as_str(), e.kind.as_str()))
                .collect();
            let mut del = tx.prepare_cached(DELETE_EDGE).map_err(db_err)?;
            for (t, k) in &stored {
                if !wanted_set.contains(&(t.as_str(), k.as_str())) {
                    del.execute(params![source, t, k]).map_err(db_err)?;
                    if k == depends_on && t.starts_with(&dependency_prefix) {
                        candidates.insert(t.clone());
                    }
                }
            }
            let stored_set: HashSet<(&str, &str)> = stored
                .iter()
                .map(|(t, k)| (t.as_str(), k.as_str()))
                .collect();
            let mut ins = tx.prepare_cached(INSERT_EDGE).map_err(db_err)?;
            for e in wanted {
                if !stored_set.contains(&(e.target.as_str(), e.kind.as_str())) {
                    ins.execute(params![e.source, e.target, e.kind.as_str()])
                        .map_err(db_err)?;
                }
            }
        }
        for (target, wanted) in &w.contained {
            let stored: Vec<String> = {
                let mut st = tx.prepare_cached(STORED_CONTAINS).map_err(db_err)?;
                let rows = st
                    .query_map([target], |r| r.get::<_, String>(0))
                    .map_err(db_err)?;
                rows.collect::<std::result::Result<_, _>>()
                    .map_err(db_err)?
            };
            note_edge_rows(stored.len());
            let contains = EdgeKind::Contains.as_str();
            let wanted_set: HashSet<&str> = wanted.iter().map(|e| e.source.as_str()).collect();
            let mut del = tx.prepare_cached(DELETE_EDGE).map_err(db_err)?;
            for s in &stored {
                if !wanted_set.contains(s.as_str()) {
                    del.execute(params![s, target, contains]).map_err(db_err)?;
                }
            }
            let stored_set: HashSet<&str> = stored.iter().map(String::as_str).collect();
            let mut ins = tx.prepare_cached(INSERT_EDGE).map_err(db_err)?;
            for e in wanted {
                if !stored_set.contains(e.source.as_str()) {
                    ins.execute(params![e.source, e.target, contains])
                        .map_err(db_err)?;
                }
            }
        }
        // Dépendances dérivées qui n'ont plus de lien `depends_on` entrant.
        for id in &candidates {
            let still_used = tx
                .prepare_cached(DEPENDS_ON_ENTERING)
                .map_err(db_err)?
                .exists([id])
                .map_err(db_err)?;
            if !still_used {
                let gone = tx
                    .prepare_cached(DELETE_DEPENDENCY)
                    .map_err(db_err)?
                    .execute([id])
                    .map_err(db_err)?;
                if gone > 0 {
                    log.deleted.insert(id.clone());
                }
            }
        }
        {
            let mut set = tx.prepare_cached(SET_UNRESOLVED).map_err(db_err)?;
            for (id, unresolved) in &w.statuses {
                set.execute(params![id, i64::from(*unresolved)])
                    .map_err(db_err)?;
            }
        }
        // Les lectures du calcul passent par la même connexion : elles voient la transaction.
        let (text, computed_at) = summary(self)?;
        set_summary_tx(&tx, &text, computed_at)?;
        set_resolution_tx(&tx, RESOLUTION_DONE)?;
        commit_counted(tx)?;
        self.stale = false;
        self.log = Some(log);
        Ok(())
    }

    /// Cibles des liens `depends_on` d'un module (résumé d'une passe incrémentale, DEC-078).
    pub(crate) fn module_dependencies(&self, module: &str) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare_cached(SUMMARY_DEPENDS_ON)
            .map_err(db_err)?;
        let rows = st
            .query_map([module], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        let out: Vec<String> = rows
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        note_edge_rows(out.len());
        Ok(out)
    }

    /// Les `limit` nœuds de plus haut degré, par agrégat SQLite (DEC-078).
    pub(crate) fn top_degrees(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        let mut st = self
            .conn
            .prepare_cached(SUMMARY_TOP_DEGREES)
            .map_err(db_err)?;
        let rows = st
            .query_map([limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })
            .map_err(db_err)?;
        let out: Vec<(String, u64)> = rows
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        note_edge_rows(out.len());
        Ok(out)
    }

    /// Nombre de mentions de chaque concept mentionné, par agrégat SQLite (DEC-078).
    pub(crate) fn concept_mentions(&self) -> Result<Vec<(String, u64)>> {
        let mut st = self.conn.prepare_cached(SUMMARY_MENTIONS).map_err(db_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })
            .map_err(db_err)?;
        let out: Vec<(String, u64)> = rows
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        note_edge_rows(out.len());
        Ok(out)
    }

    /// Références comptées non résolues par la dernière passe (index partiel, DEC-076).
    pub fn count_unresolved(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .prepare_cached(COUNT_UNRESOLVED)
            .map_err(db_err)?
            .query_row([], |r| r.get(0))
            .map_err(db_err)?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Couples (fichier, identifiant de nœud) des déclarations.
    pub fn declarations(&self) -> Result<Vec<(String, String)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT file, node FROM declarations ORDER BY file, node")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_err)?;
        rows.map(|r| r.map_err(db_err)).collect()
    }

    /// Triplets (fichier, identifiant de nœud, nom court) des déclarations, dans l'ordre
    /// (fichier, identifiant).
    pub fn declared_short_names(&self) -> Result<Vec<(String, String, String)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT file, node, short FROM declarations ORDER BY file, node")
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
        rows.map(|r| r.map_err(db_err)).collect()
    }

    /// Parmi `files`, ceux qui déclarent encore le nœud `document:<chemin>` (requête ciblée
    /// par clé primaire, sans charger toute la table des déclarations).
    pub fn files_declaring_document(&self, files: &[&str]) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT 1 FROM declarations WHERE file = ?1 AND node = ?2")
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
            .prepare_cached(
                "SELECT path, hash, size, mtime, recorded_at, state FROM files ORDER BY path",
            )
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
        match self
            .conn
            .prepare_cached("SELECT text, computed_at FROM summary WHERE id = 1")
            .map_err(db_err)?
            .query_row([], |r| Ok((r.get(0)?, r.get(1)?)))
        {
            Ok(t) => Ok(Some(t)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    pub fn set_summary(&mut self, text: &str, computed_at: i64) -> Result<()> {
        self.ensure_writable()?;
        self.end_fresh();
        set_summary_tx(&self.conn, text, computed_at)
    }
}

/// Écrit dans `tx` la différence entre les liens stockés et `edges`.
fn replace_edges_tx(tx: &rusqlite::Connection, edges: &[Edge]) -> Result<()> {
    // Seule la différence avec les liens stockés est écrite : après une mise à jour, presque
    // tous les liens sont inchangés et les réécrire tous coûterait plusieurs secondes.
    let wanted: std::collections::HashSet<(&str, &str, &str)> = edges
        .iter()
        .map(|e| (e.source.as_str(), e.target.as_str(), e.kind.as_str()))
        .collect();
    let stale: Vec<(String, String, String)> = {
        let mut st = tx
            .prepare_cached("SELECT source, target, kind FROM edges")
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(db_err)?;
        let mut stale = Vec::new();
        let mut present: std::collections::HashSet<(String, String, String)> =
            std::collections::HashSet::new();
        for row in rows {
            let (s, t, k): (String, String, String) = row.map_err(db_err)?;
            note_edge_rows(1);
            if wanted.contains(&(s.as_str(), t.as_str(), k.as_str())) {
                present.insert((s, t, k));
            } else {
                stale.push((s, t, k));
            }
        }
        drop(st);
        let mut ins = tx
            .prepare_cached("INSERT OR IGNORE INTO edges(source, target, kind) VALUES (?1, ?2, ?3)")
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
        .prepare_cached("DELETE FROM edges WHERE source = ?1 AND target = ?2 AND kind = ?3")
        .map_err(db_err)?;
    for (s, t, k) in &stale {
        del.execute(params![s, t, k]).map_err(db_err)?;
    }
    drop(del);
    Ok(())
}

/// Remplace dans `tx` les nœuds `dependency` dérivés. Renvoie ceux que l'opération crée et ceux
/// qu'elle supprime, nets (un nœud supprimé puis recréé à l'identique n'est ni l'un ni l'autre).
fn replace_dependencies_tx(tx: &rusqlite::Connection, deps: &[Node]) -> Result<ChangeLog> {
    let before: std::collections::BTreeSet<String> = {
        let mut st = tx
            .prepare_cached(
                "SELECT id FROM nodes WHERE kind = 'dependency'
                 AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        let ids: std::collections::BTreeSet<String> = rows
            .collect::<std::result::Result<_, _>>()
            .map_err(db_err)?;
        // Lecture de lignes de `nodes` (les dépendances actuelles) : comptée.
        note_node_rows(ids.len());
        ids
    };
    let mut log = ChangeLog::default();
    tx.prepare_cached(
        "DELETE FROM nodes WHERE kind = 'dependency'
             AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
    )
    .map_err(db_err)?
    .execute([])
    .map_err(db_err)?;
    {
        let mut ins = tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO nodes(id, kind, qualified, short, file, subkind, summary, qualified_rev)
                     VALUES (?1, 'dependency', ?2, ?3, NULL, NULL, NULL, ?4)",
                )
                .map_err(db_err)?;
        let mut kept = std::collections::BTreeSet::new();
        for n in deps {
            let inserted = ins
                .execute(params![
                    n.id,
                    n.qualified,
                    n.short,
                    reverse_qualified(&n.qualified)
                ])
                .map_err(db_err)?;
            if inserted > 0 {
                kept.insert(n.id.clone());
                if !before.contains(&n.id) {
                    log.inserted.insert(n.id.clone());
                }
            }
        }
        log.deleted = before.difference(&kept).cloned().collect();
    }
    Ok(log)
}

/// Dans la transaction d'une écriture de nœuds, de déclarations ou de références : écrit
/// `à résoudre` si le marqueur ne l'est pas déjà (`stale`).
fn mark_stale_tx(stale: bool, tx: &rusqlite::Connection) -> Result<()> {
    if !stale {
        set_resolution_tx(tx, RESOLUTION_PENDING)?;
    }
    Ok(())
}

/// Écrit `meta.resolution` (DEC-076).
fn set_resolution_tx(conn: &rusqlite::Connection, value: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO meta(key, value) VALUES ('resolution', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .map_err(db_err)?
    .execute([value])
    .map_err(db_err)?;
    Ok(())
}

fn set_summary_tx(conn: &rusqlite::Connection, text: &str, computed_at: i64) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO summary(id, text, computed_at) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET text = excluded.text, computed_at = excluded.computed_at",
    )
    .map_err(db_err)?
    .execute(params![text, computed_at])
    .map_err(db_err)?;
    Ok(())
}

/// Résultat de l'enrichissement d'un fichier, à écrire par `Store::apply_enrichments`.
#[derive(Debug, Clone)]
pub struct EnrichmentWrite {
    pub path: String,
    /// Empreinte du contenu enrichi.
    pub hash: String,
    /// Couples (identifiant de nœud, résumé).
    pub summaries: Vec<(String, String)>,
    pub concepts: Vec<Node>,
    pub refs: Vec<Ref>,
}

/// Écrit l'enrichissement d'un fichier dans la transaction `tx` : déclarations et références
/// `llm`, résumés, puis entrée d'enrichissement conservé.
fn write_enrichment(
    tx: &rusqlite::Transaction<'_>,
    model: &str,
    item: &EnrichmentWrite,
    track: bool,
) -> Result<ChangeLog> {
    let EnrichmentWrite {
        path,
        hash,
        summaries,
        concepts: concept_nodes,
        refs,
    } = item;
    require_known(tx, path)?;
    let mut touched = Touched::new(track);
    apply_enrichment_tx(tx, path, summaries, concept_nodes, refs, &mut touched)?;
    finish_file(tx, path, &mut touched)?;
    let payload = Payload {
        summaries: summaries.to_vec(),
        concepts: concept_nodes
            .iter()
            .map(|n| PayloadConcept {
                name: n.qualified.clone(),
                summary: n.summary.clone(),
            })
            .collect(),
        refs: refs
            .iter()
            .map(|r| PayloadRef {
                source: r.source.clone(),
                kind: r.kind.as_str().to_string(),
                target: r.target_name.clone(),
            })
            .collect(),
    };
    let json = serde_json::to_string(&payload)
        .map_err(|e| Error::Message(format!("enrichissement non sérialisable : {e}")))?;
    tx.prepare_cached(
        "INSERT INTO enrichments(path, content_hash, model, prompt_version, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET content_hash = excluded.content_hash,
             model = excluded.model, prompt_version = excluded.prompt_version,
             payload = excluded.payload",
    )
    .map_err(db_err)?
    .execute(params![path, hash, model, ENRICH_PROMPT_VERSION, json])
    .map_err(db_err)?;
    Ok(touched.log)
}

fn require_known(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<()> {
    let known: bool = tx
        .prepare_cached("SELECT 1 FROM files WHERE path = ?1")
        .map_err(db_err)?
        .query_row([path], |_| Ok(true))
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
/// nœud partagé restent) et note les nœuds concernés ; la projection est faite par `finish_file`.
fn clear_summaries(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    touched: &mut Touched,
) -> Result<()> {
    tx.prepare_cached(
        "UPDATE declarations SET summary = NULL WHERE file = ?1 AND summary IS NOT NULL",
    )
    .map_err(db_err)?
    .execute([path])
    .map_err(db_err)?;
    let declared = declared_by(tx, path)?;
    if touched.track && touched.declared_before.is_none() {
        touched.declared_before = Some(declared.iter().cloned().collect());
    }
    touched.extend(declared);
    Ok(())
}

/// Fin du travail d'un fichier (DEC-075) : projection unique de chaque nœud touché, puis
/// suppression de ceux qui n'ont plus de déclaration (liens attachés compris).
fn finish_file(tx: &rusqlite::Transaction<'_>, path: &str, touched: &mut Touched) -> Result<()> {
    let ids: Vec<&String> = touched.ids.iter().collect();
    refresh_nodes(tx, &ids)?;
    let mut deleted = Vec::new();
    {
        let mut del = tx
            .prepare_cached(
                "DELETE FROM nodes WHERE id = ?1
                 AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
            )
            .map_err(db_err)?;
        for id in &ids {
            if del.execute([id]).map_err(db_err)? > 0 {
                deleted.push((*id).clone());
            }
        }
    }
    if touched.track {
        // Différence nette des nœuds déclarés par le fichier (DEC-077 point 1).
        let before = touched.declared_before.take().unwrap_or_default();
        let after: std::collections::BTreeSet<String> =
            declared_by(tx, path)?.into_iter().collect();
        touched
            .log
            .declarations_changed
            .extend(before.symmetric_difference(&after).cloned());
        touched.log.deleted.extend(deleted);
    }
    Ok(())
}

fn declared_by(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<Vec<String>> {
    let mut st = tx
        .prepare_cached("SELECT node FROM declarations WHERE file = ?1")
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
        .prepare_cached(
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
        PROJECTIONS.with(|p| {
            *p.borrow_mut().entry(id.as_ref().to_string()).or_insert(0) += 1;
        });
    }
    Ok(())
}

fn insert_nodes(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    nodes: &[Node],
    origin: &str,
    touched: &mut Touched,
) -> Result<()> {
    let mut ins = tx
        .prepare_cached("INSERT OR IGNORE INTO nodes(id, kind, qualified, short, qualified_rev) VALUES (?1, ?2, ?3, ?4, ?5)")
        .map_err(db_err)?;
    let mut decl = tx
        .prepare_cached(
            "INSERT OR IGNORE INTO declarations(file, node, origin, short, subkind, located, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(db_err)?;
    for n in nodes {
        if n.file.as_deref().is_some_and(|f| f != path) {
            return Err(Error::Message(format!(
                "le nœud {} n'appartient pas au fichier {path}",
                n.id
            )));
        }
        let inserted = ins
            .execute(params![
                n.id,
                n.kind.as_str(),
                n.qualified,
                n.short,
                reverse_qualified(&n.qualified)
            ])
            .map_err(db_err)?;
        if inserted > 0 && touched.track {
            touched.log.inserted.insert(n.id.clone());
        }
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
        touched.insert(n.id.clone());
    }
    Ok(())
}

/// Insertion d'un nœud en base fraîche : le nœud absent est inséré avec ses attributs définitifs
/// (la projection de sa seule déclaration), le nœud déjà présent est noté pour projection.
fn insert_nodes_fresh(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    nodes: &[Node],
    origin: &str,
    touched: &mut Touched,
) -> Result<()> {
    let mut ins = tx
        .prepare_cached(
            "INSERT OR IGNORE INTO nodes(id, kind, qualified, short, file, subkind, summary, summary_file, qualified_rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(db_err)?;
    let mut decl = tx
        .prepare_cached(
            "INSERT OR IGNORE INTO declarations(file, node, origin, short, subkind, located, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(db_err)?;
    for n in nodes {
        if n.file.as_deref().is_some_and(|f| f != path) {
            return Err(Error::Message(format!(
                "le nœud {} n'appartient pas au fichier {path}",
                n.id
            )));
        }
        let inserted = ins
            .execute(params![
                n.id,
                n.kind.as_str(),
                n.qualified,
                n.short,
                n.file.as_deref().map(|_| path),
                n.subkind.map(SubKind::as_str),
                n.summary,
                n.summary.as_deref().map(|_| path),
                reverse_qualified(&n.qualified)
            ])
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
        if inserted == 0 {
            touched.insert(n.id.clone());
        }
    }
    Ok(())
}

fn insert_refs(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    refs: &[Ref],
    origin: &str,
    touched: &mut Touched,
) -> Result<()> {
    // Aucun lien `contains` n'est produit par des références : ils sont dérivés (DEC-077).
    if refs.iter().any(|r| r.kind == EdgeKind::Contains) {
        return Err(Error::Message(format!(
            "référence de genre contains refusée dans {path}"
        )));
    }
    // Contexte unique des références d'analyse du fichier (DEC-073) ; aucun pour l'origine `llm`.
    let context = refs.first().and_then(|r| r.context.as_deref());
    if origin == "llm" {
        if refs.iter().any(|r| r.context.is_some()) {
            return Err(Error::Message(format!(
                "référence d'enrichissement avec contexte d'imports dans {path}"
            )));
        }
    } else if refs.iter().any(|r| r.context.as_deref() != context) {
        return Err(Error::Message(format!(
            "références aux contextes d'imports différents dans {path}"
        )));
    }
    let mut insr = tx
        .prepare_cached(
            "INSERT INTO refs(source, file, kind, target_name, origin, target_key, target_head)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 WHERE EXISTS
               (SELECT 1 FROM declarations WHERE node = ?1 AND file = ?2)",
        )
        .map_err(db_err)?;
    for r in refs {
        let (key, head) = crate::resolve::name_keys(&r.target_name);
        let n = insr
            .execute(params![
                r.source,
                path,
                r.kind.as_str(),
                r.target_name,
                origin,
                key,
                head
            ])
            .map_err(db_err)?;
        if n == 0 {
            return Err(Error::Message(format!(
                "référence dont la source {} n'est pas déclarée dans {path}",
                r.source
            )));
        }
    }
    if touched.track && !refs.is_empty() {
        touched.log.files.insert(path.to_string());
    }
    if let Some(context) = context {
        tx.prepare_cached(
            "INSERT INTO file_contexts(file, context) VALUES (?1, ?2)
             ON CONFLICT(file) DO UPDATE SET context = excluded.context",
        )
        .map_err(db_err)?
        .execute(params![path, context])
        .map_err(db_err)?;
    }
    Ok(())
}

/// Retire les déclarations et références d'un fichier (d'une seule origine si `origin` est
/// donnée) et note les nœuds qu'il déclarait ; `finish_file` projette les nœuds partagés et
/// supprime ceux devenus orphelins (liens attachés compris). Les dépendances dérivées par la
/// résolution ne sont pas touchées.
fn release_file(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    origin: Option<&str>,
    touched: &mut Touched,
) -> Result<()> {
    // Les nœuds `dependency` dérivés (sans déclaration, DEC-020) ne sont nettoyés que par
    // `replace_derived_dependencies` ; seuls ceux que ce fichier déclarait suivent ses déclarations.
    let released: Vec<String> = {
        let mut st = tx
            .prepare_cached(
                "SELECT node FROM declarations WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![path, origin], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?
    };
    if touched.track {
        // Sources des références retirées, lues avant leur retrait (DEC-077 point 1).
        let mut st = tx
            .prepare_cached(
                "SELECT DISTINCT source FROM refs WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)",
            )
            .map_err(db_err)?;
        let sources = st
            .query_map(params![path, origin], |r| r.get::<_, String>(0))
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        if !sources.is_empty() {
            touched.log.files.insert(path.to_string());
            touched.log.removed_sources.extend(sources);
        }
    }
    tx.prepare_cached("DELETE FROM refs WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)")
        .map_err(db_err)?
        .execute(params![path, origin])
        .map_err(db_err)?;
    // Le contexte d'imports est celui des références d'analyse : l'enrichissement n'y touche pas.
    if origin != Some("llm") {
        tx.prepare_cached("DELETE FROM file_contexts WHERE file = ?1")
            .map_err(db_err)?
            .execute([path])
            .map_err(db_err)?;
    }
    tx.prepare_cached("DELETE FROM declarations WHERE file = ?1 AND (?2 IS NULL OR origin = ?2)")
        .map_err(db_err)?
        .execute(params![path, origin])
        .map_err(db_err)?;
    touched.extend(released);
    Ok(())
}

/// Condition « enrichissement conservé à jour » (`?1` = version du prompt, `?2` = modèle ou NULL).
const UP_TO_DATE: &str = "SELECT 1 FROM enrichments e WHERE e.path = f.path
    AND e.content_hash = f.hash AND e.prompt_version = ?1 AND (?2 IS NULL OR e.model = ?2)";

/// Entrée d'enrichissement conservée (DEC-062), sans son contenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentEntry {
    pub content_hash: String,
    pub model: String,
    pub prompt_version: u32,
}

#[derive(Serialize, Deserialize)]
struct PayloadConcept {
    name: String,
    summary: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct PayloadRef {
    source: String,
    kind: String,
    target: String,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    summaries: Vec<(String, String)>,
    concepts: Vec<PayloadConcept>,
    refs: Vec<PayloadRef>,
}

/// Remplace la part LLM d'un fichier : déclarations et références `llm`, résumés, concepts.
fn apply_enrichment_tx(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    summaries: &[(String, String)],
    concept_nodes: &[Node],
    refs: &[Ref],
    touched: &mut Touched,
) -> Result<()> {
    clear_summaries(tx, path, touched)?;
    release_file(tx, path, Some("llm"), touched)?;
    insert_nodes(tx, path, concept_nodes, "llm", touched)?;
    for (id, text) in summaries {
        let n = tx
            .prepare_cached("UPDATE declarations SET summary = ?2 WHERE node = ?1 AND file = ?3")
            .map_err(db_err)?
            .execute(params![id, text, path])
            .map_err(db_err)?;
        if n == 0 {
            return Err(Error::Message(format!(
                "résumé pour le nœud {id}, non déclaré dans {path}"
            )));
        }
    }
    insert_refs(tx, path, refs, "llm", touched)
}

/// Lit l'enrichissement conservé d'un fichier dont la structure vient d'être écrite : son contenu
/// si l'empreinte est celle du fichier ; une entrée de contenu différent (ou illisible) est
/// supprimée.
fn load_kept_enrichment(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<Option<Payload>> {
    let row: Option<(String, String)> = match tx
        .prepare_cached("SELECT e.content_hash, e.payload FROM enrichments e WHERE e.path = ?1")
        .map_err(db_err)?
        .query_row([path], |r| Ok((r.get(0)?, r.get(1)?)))
    {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(db_err(e)),
    };
    let Some((entry_hash, json)) = row else {
        return Ok(None);
    };
    let file_hash: String = tx
        .prepare_cached("SELECT hash FROM files WHERE path = ?1")
        .map_err(db_err)?
        .query_row([path], |r| r.get(0))
        .map_err(db_err)?;
    let payload: Option<Payload> = if entry_hash == file_hash {
        serde_json::from_str(&json).ok()
    } else {
        None
    };
    if payload.is_none() {
        tx.prepare_cached("DELETE FROM enrichments WHERE path = ?1")
            .map_err(db_err)?
            .execute([path])
            .map_err(db_err)?;
    }
    Ok(payload)
}

/// Éléments de l'enrichissement conservé qui ont un nœud correspondant (les autres sont écartés
/// en silence) : résumés, concepts, références.
fn filter_payload(
    payload: Payload,
    declared: &std::collections::HashSet<String>,
) -> (Vec<(String, String)>, Vec<Node>, Vec<Ref>) {
    let summaries: Vec<(String, String)> = payload
        .summaries
        .into_iter()
        .filter(|(id, _)| declared.contains(id))
        .collect();
    let mut refs = Vec::new();
    let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut with_refs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in payload.refs {
        with_refs.insert(r.target.clone());
        let Ok(kind) = EdgeKind::parse(&r.kind) else {
            continue;
        };
        // Un enrichissement conservé ne peut pas porter de lien `contains` (DEC-077).
        if kind == EdgeKind::Contains {
            continue;
        }
        if declared.contains(&r.source) {
            kept.insert(r.target.clone());
            refs.push(Ref {
                source: r.source,
                kind,
                target_name: r.target,
                context: None,
            });
        }
    }
    let concepts: Vec<Node> = payload
        .concepts
        .iter()
        .filter(|c| !with_refs.contains(&c.name) || kept.contains(&c.name))
        .map(|c| {
            Node::new(
                NodeKind::Concept,
                &c.name,
                &c.name,
                None,
                None,
                c.summary.as_deref(),
            )
        })
        .collect();
    (summaries, concepts, refs)
}

/// Réapplique l'enrichissement conservé d'un fichier dont la structure vient d'être écrite si
/// son empreinte de contenu est celle du fichier ; une entrée de contenu différent est
/// supprimée. Les éléments sans nœud correspondant sont écartés en silence.
fn reapply_enrichment(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    touched: &mut Touched,
) -> Result<bool> {
    let Some(payload) = load_kept_enrichment(tx, path)? else {
        return Ok(false);
    };
    let declared: std::collections::HashSet<String> = declared_by(tx, path)?.into_iter().collect();
    let (summaries, concepts, refs) = filter_payload(payload, &declared);
    apply_enrichment_tx(tx, path, &summaries, &concepts, &refs, touched)?;
    Ok(true)
}

/// Écriture de la structure d'un fichier pas encore écrit sur une base fraîche (DEC-075) : rien
/// à libérer ni à effacer ; l'enrichissement conservé est fusionné dans les enregistrements avant
/// leur insertion. Renvoie `true` si un enrichissement conservé a été réappliqué.
fn write_structure_fresh(tx: &rusqlite::Transaction<'_>, p: &PreparedStructure) -> Result<bool> {
    let path = p.path.as_str();
    // Base fraîche : le journal n'est pas tenu (DEC-077 point 1).
    let mut touched = Touched::new(false);
    let mut merged: Option<Vec<Node>> = None;
    let mut kept: Option<(Vec<Node>, Vec<Ref>)> = None;
    if let Some(payload) = load_kept_enrichment(tx, path)? {
        let declared: std::collections::HashSet<String> =
            p.nodes.iter().map(|n| n.id.clone()).collect();
        let (summaries, concepts, llm_refs) = filter_payload(payload, &declared);
        // Les résumés du fichier sont effacés puis remplacés par ceux de l'enrichissement.
        let mut parse_nodes = p.nodes.clone();
        for n in &mut parse_nodes {
            n.summary = None;
        }
        for (id, text) in &summaries {
            for n in parse_nodes.iter_mut().filter(|n| n.id == *id) {
                n.summary = Some(text.clone());
            }
        }
        merged = Some(parse_nodes);
        kept = Some((concepts, llm_refs));
    }
    insert_nodes_fresh(
        tx,
        path,
        merged.as_deref().unwrap_or(&p.nodes),
        "parse",
        &mut touched,
    )?;
    insert_prepared_refs(tx, p, &mut touched)?;
    tx.prepare_cached("UPDATE files SET state = 'indexed' WHERE path = ?1")
        .map_err(db_err)?
        .execute([path])
        .map_err(db_err)?;
    let reapplied = kept.is_some();
    if let Some((concepts, llm_refs)) = kept {
        insert_nodes_fresh(tx, path, &concepts, "llm", &mut touched)?;
        insert_refs(tx, path, &llm_refs, "llm", &mut touched)?;
    }
    let ids: Vec<&String> = touched.ids.iter().collect();
    refresh_nodes(tx, &ids)?;
    Ok(reapplied)
}

/// Insère les références d'analyse d'une structure préparée : sources déclarées, clé et tête
/// calculées, contexte unique contrôlé par la préparation (aucune sous-requête, DEC-086).
fn insert_prepared_refs(
    tx: &rusqlite::Transaction<'_>,
    p: &PreparedStructure,
    touched: &mut Touched,
) -> Result<()> {
    let path = p.path.as_str();
    {
        let mut insr = tx
            .prepare_cached(
                "INSERT INTO refs(source, file, kind, target_name, origin, target_key, target_head)
                 VALUES (?1, ?2, ?3, ?4, 'parse', ?5, ?6)",
            )
            .map_err(db_err)?;
        for r in &p.refs {
            insr.execute(params![
                r.source,
                path,
                r.kind.as_str(),
                r.target_name,
                r.key,
                r.head
            ])
            .map_err(db_err)?;
        }
    }
    if touched.track && !p.refs.is_empty() {
        touched.log.files.insert(path.to_string());
    }
    if let Some(context) = p.context.as_deref() {
        tx.prepare_cached(
            "INSERT INTO file_contexts(file, context) VALUES (?1, ?2)
             ON CONFLICT(file) DO UPDATE SET context = excluded.context",
        )
        .map_err(db_err)?
        .execute(params![path, context])
        .map_err(db_err)?;
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
        c.pragma_update(None, "user_version", 4).unwrap();
        drop(c);
        let e = Store::open_readonly(dir.path()).err().unwrap();
        assert!(e.to_string().contains("nephila build"), "{e}");
        let st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(st.files_to_enrich().unwrap().is_empty());
    }

    // --- REQ-057 : colonne qualified_rev (schéma 6) ---

    fn rev_of(q: &str) -> String {
        q.chars().rev().collect()
    }

    /// Chaque nœud porte son nom qualifié inversé ; rend le nombre de nœuds vérifiés.
    fn assert_all_rev(st: &Store) -> usize {
        let mut q = st
            .conn
            .prepare("SELECT qualified, qualified_rev FROM nodes")
            .unwrap();
        let rows: Vec<(String, String)> = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for (qualified, rev) in &rows {
            assert_eq!(rev, &rev_of(qualified), "{qualified}");
            assert!(!rev.is_empty());
        }
        rows.len()
    }

    #[test]
    fn req057_qualified_rev_is_written_by_every_node_write_path() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
        st.insert_pending_files(&[file("a.md"), file("b.md")])
            .unwrap();
        // Structure d'un fichier, noms non ASCII compris.
        st.replace_structure(
            "a.md",
            &[ty("p.Écran.Tâche", "a.md"), ty("p.A", "a.md")],
            &[],
        )
        .unwrap();
        // Nœud partagé déclaré par deux fichiers.
        let shared = Node::new(NodeKind::Package, "p.partagé", "partagé", None, None, None);
        st.replace_structure("b.md", std::slice::from_ref(&shared), &[])
            .unwrap();
        // Dépendances dérivées.
        let dep = Node::new(NodeKind::Dependency, "org.ünï:lib", "lib", None, None, None);
        st.replace_derived_dependencies(&[dep]).unwrap();
        assert!(assert_all_rev(&st) >= 4);
        // Mise à jour d'un nœud : la colonne reste juste.
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        assert!(assert_all_rev(&st) >= 3);
        let rev: String = st
            .conn
            .query_row(
                "SELECT qualified_rev FROM nodes WHERE id = 'dependency:org.ünï:lib'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, "bil:ïnü.gro");
    }

    #[test]
    fn req057_ac1_resolution_queries_use_indexes() {
        let dir = tempdir().unwrap();
        let st = Store::open_or_create(dir.path()).unwrap();
        let plans = [
            (RESOLVE_ID, 1, None),
            (RESOLVE_QUALIFIED, 1, None),
            (RESOLVE_SHORT, 1, None),
            (RESOLVE_SUFFIX, 2, Some("idx_nodes_qualified_rev")),
        ];
        for (sql, n, index) in plans {
            let params: Vec<&str> = ["a", "b"][..n].to_vec();
            let mut stmt = st
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let details: Vec<String> = stmt
                .query_map(rusqlite::params_from_iter(params), |r| r.get(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(!details.is_empty());
            for d in &details {
                assert!(!d.contains("SCAN nodes"), "{sql} : {d}");
            }
            if let Some(i) = index {
                assert!(details.iter().any(|d| d.contains(i)), "{details:?}");
            }
        }
    }

    /// Assistant « schéma 6 de la 0.20.0 » (DEC-073 point 6), en SQL écrit en dur : `refs`
    /// recréée avec `AUTOINCREMENT` et `context` rempli depuis `file_contexts`, index de la
    /// 0.20.0, `file_contexts` et `idx_declarations_node_file` supprimés, versions 6.
    const SCHEMA6_DOWNGRADE: &str = "BEGIN;
        CREATE TABLE refs_old (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            target_name TEXT NOT NULL,
            context TEXT,
            origin TEXT NOT NULL DEFAULT 'parse' CHECK (origin IN ('parse', 'llm'))
        );
        INSERT INTO refs_old(id, source, file, kind, target_name, context, origin)
            SELECT r.id, r.source, r.file, r.kind, r.target_name,
                   (SELECT c.context FROM file_contexts c WHERE c.file = r.file AND r.origin = 'parse'),
                   r.origin
            FROM refs r ORDER BY r.id;
        DROP TABLE refs;
        ALTER TABLE refs_old RENAME TO refs;
        CREATE INDEX idx_refs_source ON refs(source);
        CREATE INDEX idx_refs_file ON refs(file);
        CREATE INDEX idx_refs_target ON refs(target_name);
        CREATE INDEX idx_edges_source ON edges(source);
        CREATE INDEX idx_declarations_node ON declarations(node);
        DROP INDEX idx_declarations_node_file;
        DROP TABLE file_contexts;
        UPDATE meta SET value = '6' WHERE key = 'schema_version';
        PRAGMA user_version = 6;
        COMMIT;";

    /// Assistant « schéma 7 de la 0.21.0 » (DEC-076 point 6), en SQL écrit en dur : index
    /// nouveaux supprimés, trois colonnes de `refs` retirées, clé `meta.resolution` supprimée,
    /// versions 7.
    const SCHEMA7_DOWNGRADE: &str = "BEGIN;
        DROP INDEX idx_refs_key;
        DROP INDEX idx_refs_head;
        DROP INDEX idx_refs_unresolved;
        ALTER TABLE refs DROP COLUMN target_key;
        ALTER TABLE refs DROP COLUMN target_head;
        ALTER TABLE refs DROP COLUMN unresolved;
        DELETE FROM meta WHERE key = 'resolution';
        UPDATE meta SET value = '7' WHERE key = 'schema_version';
        PRAGMA user_version = 7;
        COMMIT;";

    /// Ramène un graphe de schéma 8 au schéma 7 de la 0.21.0.
    fn downgrade_to_7(repo: &Path) {
        Connection::open(db_path(repo))
            .unwrap()
            .execute_batch(SCHEMA7_DOWNGRADE)
            .unwrap();
    }

    /// Ramène un graphe de schéma 8 au schéma 6 de la 0.20.0 (le schéma 7 d'abord).
    fn downgrade_to_6(repo: &Path) {
        downgrade_to_7(repo);
        Connection::open(db_path(repo))
            .unwrap()
            .execute_batch(SCHEMA6_DOWNGRADE)
            .unwrap();
    }

    /// Ramène un graphe de schéma 8 au schéma 5 (6, puis 6 -> 5).
    fn downgrade_to_5(repo: &Path) {
        downgrade_to_6(repo);
        let c = Connection::open(db_path(repo)).unwrap();
        c.execute_batch(
            "DROP INDEX idx_nodes_qualified_rev; ALTER TABLE nodes DROP COLUMN qualified_rev;
             UPDATE meta SET value = '5' WHERE key = 'schema_version'; PRAGMA user_version = 5;",
        )
        .unwrap();
    }

    fn dump(st: &Store, table: &str, cols: &str) -> Vec<Vec<Option<String>>> {
        let n = cols.split(',').count();
        let mut q = st
            .conn
            .prepare(&format!("SELECT {cols} FROM {table} ORDER BY 1, 2"))
            .unwrap();
        q.query_map([], |r| {
            (0..n)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => None,
                        v => Some(format!("{v:?}")),
                    })
                })
                .collect()
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    fn populated(repo: &Path) -> Store {
        let mut st = Store::open_or_create(repo).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        st.replace_structure("a.md", &[ty("p.Écran", "a.md"), ty("p.B", "a.md")], &[])
            .unwrap();
        st.replace_derived_dependencies(&[Node::new(
            NodeKind::Dependency,
            "g:lib",
            "lib",
            None,
            None,
            None,
        )])
        .unwrap();
        st
    }

    const SNAP: [(&str, &str); 4] = [
        ("files", "path,hash,size,mtime,recorded_at,state"),
        (
            "nodes",
            "id,kind,qualified,short,file,subkind,summary,summary_file",
        ),
        ("edges", "source,target,kind"),
        (
            "enrichments",
            "path,content_hash,model,prompt_version,payload",
        ),
    ];

    fn snapshot5(st: &Store) -> Vec<Vec<Vec<Option<String>>>> {
        let mut out: Vec<Vec<Vec<Option<String>>>> =
            SNAP.iter().map(|(t, c)| dump(st, t, c)).collect();
        // Les références (contexte compris) se lisent par l'API, quel que soit le schéma.
        out.push(
            st.refs_with_origin()
                .unwrap()
                .into_iter()
                .map(|(r, o)| {
                    vec![
                        Some(r.source),
                        Some(r.kind.as_str().to_string()),
                        Some(r.target_name),
                        r.context,
                        Some(format!("{o:?}")),
                    ]
                })
                .collect(),
        );
        out
    }

    #[test]
    fn req057_schema5_is_migrated_in_place_by_open_or_create_and_open_existing() {
        for existing in [false, true] {
            let dir = tempdir().unwrap();
            drop(populated(dir.path()));
            downgrade_to_5(dir.path());
            let before = {
                let st = Store::open_readonly(dir.path()).unwrap();
                snapshot5(&st)
            };
            let st = if existing {
                Store::open_existing(dir.path()).unwrap()
            } else {
                Store::open_or_create(dir.path()).unwrap()
            };
            assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
            assert_eq!(assert_all_rev(&st), 3);
            let idx: i64 = st
                .conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_nodes_qualified_rev'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(idx, 1);
            let v: String = st
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION.to_string());
            assert_eq!(snapshot5(&st), before);
        }
    }

    #[test]
    fn req057_open_readonly_accepts_schema5_without_touching_the_file() {
        let dir = tempdir().unwrap();
        drop(populated(dir.path()));
        downgrade_to_5(dir.path());
        let bytes = std::fs::read(db_path(dir.path())).unwrap();
        let st = Store::open_readonly(dir.path()).unwrap();
        assert_eq!(st.schema_version().unwrap(), 5);
        assert_eq!(st.count_nodes().unwrap(), 3);
        drop(st);
        assert_eq!(st_user_version(dir.path()), 5);
        assert_eq!(std::fs::read(db_path(dir.path())).unwrap(), bytes);
    }

    fn st_user_version(repo: &Path) -> i32 {
        Connection::open(db_path(repo))
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn req057_schema4_still_recreated_by_build_and_refused_by_open_existing() {
        let dir = tempdir().unwrap();
        drop(populated(dir.path()));
        Connection::open(db_path(dir.path()))
            .unwrap()
            .pragma_update(None, "user_version", 4)
            .unwrap();
        assert!(Store::open_existing(dir.path()).is_err());
        let st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(st.count_nodes().unwrap(), 0);
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
            "m",
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
        // Le fichier est modifié puis réanalysé : résumés, concepts, références llm et marque
        // disparaissent (un contenu inchangé réappliquerait l'enrichissement conservé).
        let mut modified = file("a.md");
        modified.hash = "h2".into();
        st.insert_files(&[modified]).unwrap();
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
            "m",
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
            "m",
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
            st.apply_enrichment(f, "h", "m", &[], &[concept("c")], &[mention(s, "c")])
                .unwrap();
        }
        assert_eq!(st.nodes().unwrap().len(), 3);
        // a.md est ré-enrichi sans le concept : b.md le déclare encore.
        st.apply_enrichment("a.md", "h", "m", &[], &[], &[])
            .unwrap();
        assert!(st.nodes().unwrap().iter().any(|n| n.id == "concept:c"));
        assert_eq!(st.refs().unwrap().len(), 1);
        st.apply_enrichment("b.md", "h", "m", &[], &[], &[])
            .unwrap();
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
        st.apply_enrichment("a.md", "autre", "m", &[], &[], &[])
            .unwrap();
        assert_eq!(paths(&st), vec!["a.md"]);
        st.apply_enrichment("a.md", "h", "m", &[], &[], &[])
            .unwrap();
        assert!(paths(&st).is_empty());
        // Contenu modifié : l'empreinte diffère à nouveau.
        let mut f = file("a.md");
        f.hash = "h2".into();
        st.insert_files(&[f]).unwrap();
        assert_eq!(paths(&st), vec!["a.md"]);
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
    }

    fn enriched_store() -> (tempfile::TempDir, Store) {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        st.apply_enrichment(
            "a.md",
            "h",
            "m",
            &[("type:p.A".into(), "résumé".into())],
            &[concept("idée")],
            &[mention("type:p.A", "idée")],
        )
        .unwrap();
        (dir, st)
    }

    #[test]
    fn req049_entry_is_written_by_apply_enrichment_and_survives_reset() {
        let (_d, mut st) = enriched_store();
        let e = st.enrichment_entry("a.md").unwrap().unwrap();
        assert_eq!(
            (e.content_hash.as_str(), e.model.as_str(), e.prompt_version),
            ("h", "m", ENRICH_PROMPT_VERSION)
        );
        st.reset().unwrap();
        assert!(st.enrichment_entry("a.md").unwrap().is_some());
        st.reset_with_pending_files(&[file("a.md")]).unwrap();
        assert!(st.enrichment_entry("a.md").unwrap().is_some());
        assert!(st.nodes().unwrap().is_empty());
    }

    #[test]
    fn req049_entry_is_reapplied_when_the_content_is_unchanged() {
        let (_d, mut st) = enriched_store();
        let before = (st.nodes().unwrap(), st.refs_with_origin().unwrap().len());
        st.reset_with_pending_files(&[file("a.md")]).unwrap();
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        let mut after = st.nodes().unwrap();
        let mut b = before.0;
        after.sort_by(|x, y| x.id.cmp(&y.id));
        b.sort_by(|x, y| x.id.cmp(&y.id));
        assert_eq!(after, b);
        assert_eq!(st.refs_with_origin().unwrap().len(), before.1);
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
    }

    #[test]
    fn req049_reapplication_drops_elements_without_a_node_silently() {
        let (_d, mut st) = enriched_store();
        st.reset_with_pending_files(&[file("a.md")]).unwrap();
        // L'analyseur ne déclare plus `p.A` : résumé, mention et concept sont écartés.
        st.replace_structure("a.md", &[ty("p.Autre", "a.md")], &[])
            .unwrap();
        let nodes = st.nodes().unwrap();
        assert!(nodes.iter().all(|n| n.summary.is_none()));
        assert!(!nodes.iter().any(|n| n.kind == NodeKind::Concept));
        assert!(st.refs().unwrap().is_empty());
    }

    #[test]
    fn req049_entry_of_changed_content_is_deleted() {
        let (_d, mut st) = enriched_store();
        let mut modified = file("a.md");
        modified.hash = "h2".into();
        st.reset_with_pending_files(&[modified]).unwrap();
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        assert_eq!(st.enrichment_entry("a.md").unwrap(), None);
        assert!(st.nodes().unwrap().iter().all(|n| n.summary.is_none()));
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
    }

    #[test]
    fn req049_entry_of_an_unindexed_path_is_pruned() {
        let (_d, mut st) = enriched_store();
        st.reset_with_pending_files(&[]).unwrap();
        assert!(st.enrichment_entry("a.md").unwrap().is_some());
        assert_eq!(st.prune_enrichments().unwrap(), 1);
        assert_eq!(st.enrichment_entry("a.md").unwrap(), None);
        // `remove_file` supprime aussi l'entrée.
        let (_d, mut st) = enriched_store();
        st.remove_file("a.md").unwrap();
        assert_eq!(st.enrichment_entry("a.md").unwrap(), None);
    }

    #[test]
    fn req049_to_enrich_compares_hash_model_and_prompt_version() {
        let (_d, mut st) = enriched_store();
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
        st.set_enrich_model("m").unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
        st.set_enrich_model("autre").unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
        assert_eq!(st.files_to_enrich().unwrap().len(), 1);
        st.set_enrich_model("m").unwrap();
        st.conn
            .execute(
                "UPDATE enrichments SET prompt_version = prompt_version + 1",
                [],
            )
            .unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
    }

    #[test]
    fn req049_ac4_to_enrich_changes_with_content_model_or_prompt_version_only() {
        let (_d, mut st) = enriched_store();
        st.set_enrich_model("m").unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
        // Autres réglages (charge utile, métadonnées du fichier, autre clé de meta) : sans effet.
        st.conn
            .execute("UPDATE enrichments SET payload = '{}'", [])
            .unwrap();
        st.conn
            .execute("UPDATE files SET size = size + 1, mtime = mtime + 1", [])
            .unwrap();
        st.conn
            .execute(
                "INSERT INTO meta(key, value) VALUES ('autre', 'x')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
        assert!(st.files_to_enrich().unwrap().is_empty());
        // Le contenu, le modèle ou la version du prompt changent : le fichier est à enrichir.
        st.set_enrich_model("autre").unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
        st.set_enrich_model("m").unwrap();
        st.conn.execute("UPDATE files SET hash = 'h2'", []).unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 1);
        st.conn.execute("UPDATE files SET hash = 'h'", []).unwrap();
        assert_eq!(st.count_files_to_enrich().unwrap(), 0);
        st.conn
            .execute("UPDATE enrichments SET prompt_version = 99", [])
            .unwrap();
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
            "m",
            &[],
            &[concept("c")],
            &[mention("type:p.A", "c")],
        )
        .unwrap();
        let r = st.apply_enrichment(
            "a.md",
            "h2",
            "m",
            &[("type:inconnu".into(), "x".into())],
            &[],
            &[],
        );
        assert!(r.is_err());
        assert_eq!(st.enriched_hash("a.md").unwrap().as_deref(), Some("h"));
        assert_eq!(st.refs().unwrap().len(), 1);
        assert!(st
            .apply_enrichment("absent.md", "h", "m", &[], &[], &[])
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
                st.apply_enrichment(
                    f,
                    "h",
                    "m",
                    &[("type:p.A".into(), format!("res-{f}"))],
                    &[],
                    &[],
                )
                .unwrap();
            }
            let n = &st.nodes().unwrap()[0];
            assert_eq!(n.summary.as_deref(), Some("res-a"));
            // Modification et réanalyse de `a` : son résumé disparaît, celui de `b` apparaît.
            let mut modified = file("a");
            modified.hash = "h2".into();
            st.insert_files(&[modified]).unwrap();
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
            st.apply_enrichment(
                f,
                "h",
                "m",
                &[("concept:c".into(), format!("r-{f}"))],
                &[c],
                &[],
            )
            .unwrap();
        }
        let n = &st.nodes().unwrap()[0];
        assert_eq!(n.file, None);
        assert_eq!(n.summary.as_deref(), Some("r-a.md"));
    }

    fn pragma_i64(st: &Store, name: &str) -> i64 {
        st.conn
            .pragma_query_value(None, name, |r| r.get(0))
            .unwrap()
    }

    fn assert_write_pragmas(st: &Store) {
        assert_eq!(pragma_i64(st, "synchronous"), 1);
        assert_eq!(pragma_i64(st, "cache_size"), -262_144);
        assert_eq!(pragma_i64(st, "temp_store"), 2);
        assert_eq!(pragma_i64(st, "mmap_size"), 268_435_456);
        assert_eq!(st.journal_mode().unwrap(), "wal");
        assert_eq!(pragma_i64(st, "foreign_keys"), 1);
    }

    #[test]
    fn req059_ac1_write_openings_set_the_six_pragmas() {
        let dir = tempdir().unwrap();
        assert_write_pragmas(&Store::open_or_create(dir.path()).unwrap());
        assert_write_pragmas(&Store::open_or_create(dir.path()).unwrap());
        assert_write_pragmas(&Store::open_existing(dir.path()).unwrap());
    }

    #[test]
    fn req059_ac2_pragmas_hold_after_migration_of_older_schemas() {
        let dir = tempdir().unwrap();
        drop(populated(dir.path()));
        assert_write_pragmas(&Store::open_existing(dir.path()).unwrap());
        assert_write_pragmas(&Store::open_or_create(dir.path()).unwrap());
        downgrade_to_6(dir.path());
        assert_write_pragmas(&Store::open_existing(dir.path()).unwrap());
        downgrade_to_6(dir.path());
        assert_write_pragmas(&Store::open_or_create(dir.path()).unwrap());
        downgrade_to_5(dir.path());
        assert_write_pragmas(&Store::open_existing(dir.path()).unwrap());
        downgrade_to_5(dir.path());
        assert_write_pragmas(&Store::open_or_create(dir.path()).unwrap());
    }

    /// Code hors tests de `mod.rs`, blancs réduits à une espace.
    fn production_source_reduced() -> String {
        let src = include_str!("mod.rs");
        let cut = src
            .match_indices("#[cfg(test)]")
            .find(|(i, _)| *i == 0 || src.as_bytes()[i - 1] == b'\n')
            .map(|(i, _)| i)
            .unwrap();
        src[..cut].split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Occurrences de `.prepare` suivi (blancs admis) de `(`.
    fn prepare_calls(text: &str) -> usize {
        let mut n = 0;
        for (i, _) in text.match_indices("prepare") {
            let dotted = text[..i].trim_end().ends_with('.');
            if dotted && text[i + "prepare".len()..].trim_start().starts_with('(') {
                n += 1;
            }
        }
        n
    }

    /// Occurrences de `conn|tx` (frontière de mot) `.` `execute|query_row|query_map` `(`.
    fn direct_calls(text: &str) -> usize {
        let b = text.as_bytes();
        let mut n = 0;
        for word in ["conn", "tx"] {
            for (i, _) in text.match_indices(word) {
                let before_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
                if !before_ok {
                    continue;
                }
                let Some(rest) = text[i + word.len()..].trim_start().strip_prefix('.') else {
                    continue;
                };
                let rest = rest.trim_start();
                for m in ["execute", "query_row", "query_map"] {
                    if let Some(r) = rest.strip_prefix(m) {
                        if r.trim_start().starts_with('(') {
                            n += 1;
                        }
                    }
                }
            }
        }
        n
    }

    #[test]
    fn req061_ac1_no_uncached_sql_in_production_code() {
        let text = production_source_reduced();
        assert_eq!(prepare_calls(&text), 0, "`.prepare(` hors cache");
        assert_eq!(
            direct_calls(&text),
            0,
            "execute/query_row/query_map directs"
        );
        assert_eq!(prepare_calls("a . prepare ( x"), 1);
        assert_eq!(direct_calls("self.conn . execute ( x"), 1);
        assert_eq!(direct_calls("tx.prepare_cached(x).execute(y)"), 0);
    }

    #[test]
    fn diagnostics_count_commits_and_projections() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        reset_diagnostics();
        assert_eq!(diagnostics().commits(), 0);
        st.replace_structure("a.md", &[ty("p.A", "a.md")], &[])
            .unwrap();
        st.apply_enrichment(
            "a.md",
            "h",
            "m",
            &[(node_id(NodeKind::Type, "p.A"), "r".to_string())],
            &[],
            &[],
        )
        .unwrap();
        let d = diagnostics();
        assert_eq!(d.commits(), 2);
        assert!(d.total_projections() >= 2);
        assert!(d.projections(&node_id(NodeKind::Type, "p.A")) >= 2);
        reset_diagnostics();
        assert_eq!(diagnostics(), Diagnostics::default());
    }

    // --- REQ-062, REQ-063 : schéma 7, contexte d'imports par fichier ---

    const CTX: &str = "package p;\nimport q.R;";

    fn pref(src: &str, target: &str, ctx: Option<&str>) -> Ref {
        Ref {
            source: src.into(),
            kind: EdgeKind::Imports,
            target_name: target.into(),
            context: ctx.map(String::from),
        }
    }

    /// Graphe peuplé : fichiers, nœud partagé, références d'analyse avec contextes, références
    /// `llm` sans contexte (insérées après celles de B), liens, résumés, résumé global.
    fn rich(repo: &Path) -> Store {
        let mut st = Store::open_or_create(repo).unwrap();
        st.insert_pending_files(&[file("A.java"), file("B.java"), file("C.md")])
            .unwrap();
        let (a, b, c, shared) = (
            ty("p.A", "A.java"),
            ty("p.B", "B.java"),
            ty("p.C", "C.md"),
            ty("p.Shared", "A.java"),
        );
        let mut shared_b = shared.clone();
        shared_b.file = Some("B.java".into());
        st.replace_structures(&[
            (
                "A.java".to_string(),
                vec![a.clone(), shared.clone()],
                vec![
                    pref(&a.id, "R", Some(CTX)),
                    pref(&a.id, "S", Some(CTX)),
                    pref(&shared.id, "R", Some(CTX)),
                ],
            ),
            (
                "B.java".to_string(),
                vec![b.clone(), shared_b],
                vec![
                    pref(&b.id, "R", Some("package p;")),
                    pref(&shared.id, "T", Some("package p;")),
                ],
            ),
            (
                "C.md".to_string(),
                vec![c.clone()],
                vec![pref(&c.id, "Z", None)],
            ),
        ])
        .unwrap();
        st.apply_enrichment(
            "A.java",
            "h",
            "m",
            &[(a.id.clone(), "résumé de A".to_string())],
            &[concept("facturation")],
            &[mention(&a.id, "facturation"), mention(&a.id, "autre")],
        )
        .unwrap();
        st.replace_edges(&[Edge {
            source: a.id.clone(),
            target: b.id.clone(),
            kind: EdgeKind::Imports,
        }])
        .unwrap();
        st.set_summary("résumé global", 42).unwrap();
        st
    }

    type Rows = Vec<Vec<Option<String>>>;
    type Snapshot7 = (Vec<Rows>, Rows, Vec<(Ref, RefOrigin)>);

    fn objects(st: &Store) -> std::collections::BTreeSet<(String, String, String)> {
        let mut q = st
            .conn
            .prepare(
                "SELECT type, name, tbl_name FROM sqlite_master WHERE name <> 'sqlite_sequence'",
            )
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// Tables conservées par la migration (meta hors version), refs par l'API et par identifiant.
    fn snapshot7(st: &Store) -> Snapshot7 {
        let tables = [
            ("files", "path,hash,size,mtime,recorded_at,state"),
            (
                "nodes",
                "id,kind,qualified,short,file,subkind,summary,summary_file,qualified_rev",
            ),
            (
                "declarations",
                "file,node,origin,short,subkind,located,summary",
            ),
            ("edges", "source,target,kind"),
            (
                "enrichments",
                "path,content_hash,model,prompt_version,payload",
            ),
            ("summary", "id,text,computed_at"),
            (
                "(SELECT * FROM meta WHERE key NOT IN ('schema_version', 'resolution'))",
                "key,value",
            ),
        ];
        let ids = dump(st, "refs", "id,source,file,kind,target_name,origin");
        (
            tables.iter().map(|(t, c)| dump(st, t, c)).collect(),
            ids,
            st.refs_with_origin().unwrap(),
        )
    }

    #[test]
    fn req062_ac1_fresh_graph_has_the_schema_7_objects() {
        let dir = tempdir().unwrap();
        let st = Store::open_or_create(dir.path()).unwrap();
        let names: Vec<String> = objects(&st).into_iter().map(|o| o.1).collect();
        for gone in [
            "idx_refs_target",
            "idx_edges_source",
            "idx_declarations_node",
        ] {
            assert!(!names.iter().any(|n| n == gone), "{gone}");
        }
        assert!(names.iter().any(|n| n == "idx_declarations_node_file"));
        assert!(names.iter().any(|n| n == "file_contexts"));
        let cols: Vec<String> = {
            let mut q = st
                .conn
                .prepare("SELECT name FROM pragma_index_info('idx_declarations_node_file') ORDER BY seqno")
                .unwrap();
            q.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(cols, ["node", "file"]);
        let refs_sql: String = st
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'refs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !refs_sql.to_uppercase().contains("AUTOINCREMENT"),
            "{refs_sql}"
        );
        let ref_cols: Vec<String> = {
            let mut q = st
                .conn
                .prepare("SELECT name FROM pragma_table_info('refs')")
                .unwrap();
            q.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(!ref_cols.iter().any(|c| c == "context"), "{ref_cols:?}");
        assert_eq!(st_user_version(dir.path()), SCHEMA_VERSION);
        let v: String = st
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
        assert_eq!(SCHEMA_VERSION, 8);
    }

    #[test]
    fn req062_ac3_new_ref_ids_exceed_every_present_id() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        let files = ["a.java", "b.java", "c.java", "d.java"];
        st.insert_pending_files(&files.map(file)).unwrap();
        // Plus grand identifiant des références des autres fichiers : celles du fichier réécrit
        // sont supprimées avant l'insertion des nouvelles.
        let max_id = |st: &Store, f: &str| -> i64 {
            st.conn
                .query_row(
                    "SELECT coalesce(max(id), 0) FROM refs WHERE file <> ?1",
                    [f],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let ids = |st: &Store, f: &str| -> Vec<i64> {
            let mut q = st
                .conn
                .prepare("SELECT id FROM refs WHERE file = ?1 ORDER BY id")
                .unwrap();
            q.query_map([f], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        let write = |st: &mut Store, f: &str, tag: &str| {
            let n = ty(&format!("p.{tag}"), f);
            let refs: Vec<Ref> = (0..3)
                .map(|i| pref(&n.id, &format!("{tag}{i}"), None))
                .collect();
            let before = max_id(st, f);
            st.replace_structure(f, &[n], &refs).unwrap();
            let got = ids(st, f);
            assert_eq!(got.len(), 3);
            assert!(got.iter().all(|i| *i > before), "{got:?} > {before}");
        };
        write(&mut st, "a.java", "A");
        write(&mut st, "b.java", "B");
        write(&mut st, "c.java", "C");
        // Le dernier écrit est remplacé : ses identifiants sont libérés, les nouveaux dépassent
        // tous ceux qui restent.
        write(&mut st, "c.java", "C2");
        write(&mut st, "d.java", "D");
        let order: Vec<String> = st
            .refs()
            .unwrap()
            .into_iter()
            .map(|r| r.target_name)
            .collect();
        let expected: Vec<String> = ["A", "B", "C2", "D"]
            .iter()
            .flat_map(|t| (0..3).map(move |i| format!("{t}{i}")))
            .collect();
        assert_eq!(order, expected);
    }

    fn migrated_from(version: u32, existing: bool) -> (tempfile::TempDir, Store, Snapshot7) {
        let dir = tempdir().unwrap();
        let fresh_objects;
        let before = {
            let st = rich(dir.path());
            fresh_objects = objects(&st);
            snapshot7(&st)
        };
        match version {
            5 => downgrade_to_5(dir.path()),
            6 => downgrade_to_6(dir.path()),
            _ => downgrade_to_7(dir.path()),
        }
        let st = if existing {
            Store::open_existing(dir.path()).unwrap()
        } else {
            Store::open_or_create(dir.path()).unwrap()
        };
        assert_eq!(objects(&st), fresh_objects);
        (dir, st, before)
    }

    #[test]
    fn req062_ac4_schema6_and_schema5_are_migrated_in_place() {
        for version in [6, 5] {
            for existing in [false, true] {
                let (dir, st, before) = migrated_from(version, existing);
                assert_eq!(
                    st.schema_version().unwrap(),
                    SCHEMA_VERSION,
                    "depuis {version}"
                );
                assert_eq!(st_user_version(dir.path()), SCHEMA_VERSION);
                let v: String = st
                    .conn
                    .query_row(
                        "SELECT value FROM meta WHERE key = 'schema_version'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(v, SCHEMA_VERSION.to_string());
                assert_eq!(
                    snapshot7(&st),
                    before,
                    "depuis {version}, existing={existing}"
                );
                let seq: i64 = st
                    .conn
                    .query_row(
                        "SELECT count(*) FROM sqlite_master WHERE name = 'sqlite_sequence'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                if seq == 1 {
                    let rows: i64 = st
                        .conn
                        .query_row(
                            "SELECT count(*) FROM sqlite_sequence WHERE name = 'refs'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(rows, 0);
                }
                assert!(before.2.iter().any(|(r, _)| r.context.is_some()));
            }
        }
    }

    #[test]
    fn req062_open_readonly_accepts_schemas_5_to_8_without_touching_the_file() {
        for version in [8, 7, 6, 5] {
            let dir = tempdir().unwrap();
            let before = snapshot7(&rich(dir.path()));
            match version {
                7 => downgrade_to_7(dir.path()),
                6 => downgrade_to_6(dir.path()),
                5 => downgrade_to_5(dir.path()),
                _ => {}
            }
            let bytes = std::fs::read(db_path(dir.path())).unwrap();
            let st = Store::open_readonly(dir.path()).unwrap();
            assert_eq!(st.schema_version().unwrap(), version);
            assert_eq!(st.refs_with_origin().unwrap(), before.2, "schéma {version}");
            assert_eq!(
                st.refs().unwrap(),
                before.2.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>()
            );
            drop(st);
            assert_eq!(st_user_version(dir.path()), version);
            assert_eq!(std::fs::read(db_path(dir.path())).unwrap(), bytes);
        }
    }

    fn contexts(st: &Store) -> Vec<(String, String)> {
        let mut q = st
            .conn
            .prepare("SELECT file, context FROM file_contexts ORDER BY file")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn req063_ac1_one_context_row_per_file() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("A.java"), file("B.java")])
            .unwrap();
        let a = ty("p.A", "A.java");
        let refs: Vec<Ref> = ["x", "y", "z"]
            .iter()
            .map(|t| pref(&a.id, t, Some(CTX)))
            .collect();
        st.replace_structure("A.java", std::slice::from_ref(&a), &refs)
            .unwrap();
        st.apply_enrichment(
            "A.java",
            "h",
            "m",
            &[],
            &[concept("c")],
            &[mention(&a.id, "c"), mention(&a.id, "d")],
        )
        .unwrap();
        assert_eq!(contexts(&st), [("A.java".to_string(), CTX.to_string())]);
        let all = st.refs_with_origin().unwrap();
        assert_eq!(all.len(), 5);
        for (i, (r, o)) in all.iter().enumerate() {
            if i < 3 {
                assert_eq!(
                    (r.context.as_deref(), *o),
                    (Some(CTX), RefOrigin::Parse),
                    "{i}"
                );
            } else {
                assert_eq!((r.context.as_deref(), *o), (None, RefOrigin::Llm), "{i}");
            }
        }
        // Enrichissement remplacé : la ligne n'est pas touchée.
        st.apply_enrichment("A.java", "h", "m", &[], &[], &[mention(&a.id, "e")])
            .unwrap();
        assert_eq!(contexts(&st), [("A.java".to_string(), CTX.to_string())]);
        // Nouvelle structure (même empreinte) : l'enrichissement conservé est réappliqué, la
        // ligne est identique.
        st.replace_structure("A.java", std::slice::from_ref(&a), &refs)
            .unwrap();
        assert_eq!(contexts(&st), [("A.java".to_string(), CTX.to_string())]);
        // Références sans contexte : la ligne disparaît.
        let plain: Vec<Ref> = vec![pref(&a.id, "x", None)];
        st.replace_structure("A.java", std::slice::from_ref(&a), &plain)
            .unwrap();
        assert!(contexts(&st).is_empty());
        // Suppression du fichier.
        st.replace_structure("A.java", std::slice::from_ref(&a), &refs)
            .unwrap();
        assert_eq!(contexts(&st).len(), 1);
        st.remove_file("A.java").unwrap();
        assert!(contexts(&st).is_empty());
        // Réinitialisation.
        st.insert_pending_files(&[file("B.java")]).unwrap();
        let b = ty("p.B", "B.java");
        st.replace_structure(
            "B.java",
            std::slice::from_ref(&b),
            &[pref(&b.id, "x", Some("package b;"))],
        )
        .unwrap();
        assert_eq!(contexts(&st).len(), 1);
        st.reset().unwrap();
        assert!(contexts(&st).is_empty());
    }

    #[test]
    fn req063_ac2_inconsistent_contexts_are_refused_and_leave_the_graph_unchanged() {
        let dir = tempdir().unwrap();
        let mut st = rich(dir.path());
        let before = snapshot7(&st);
        let ctxs = contexts(&st);
        let a = ty("p.A", "A.java");
        for refs in [
            vec![
                pref(&a.id, "x", Some("package a;")),
                pref(&a.id, "y", Some("package b;")),
            ],
            vec![pref(&a.id, "x", Some("package a;")), pref(&a.id, "y", None)],
            vec![pref(&a.id, "x", None), pref(&a.id, "y", Some("package a;"))],
        ] {
            let e = st
                .replace_structure("A.java", std::slice::from_ref(&a), &refs)
                .unwrap_err();
            assert!(
                e.to_string()
                    .contains("références aux contextes d'imports différents dans A.java"),
                "{e}"
            );
            assert_eq!(snapshot7(&st), before);
            assert_eq!(contexts(&st), ctxs);
        }
        let e = st
            .apply_enrichment(
                "A.java",
                "h",
                "m",
                &[],
                &[],
                &[pref(&a.id, "x", Some("package a;"))],
            )
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("référence d'enrichissement avec contexte d'imports dans A.java"),
            "{e}"
        );
        assert_eq!(snapshot7(&st), before);
        assert_eq!(contexts(&st), ctxs);
    }
    // --- REQ-064 : base fraîche, une projection par nœud touché ---

    const NODES_COLS: &str =
        "id,kind,qualified,short,file,subkind,summary,summary_file,qualified_rev";

    /// Tables comparées entre chemin base fraîche et chemin général.
    fn graph_dump(st: &Store) -> Vec<Rows> {
        vec![
            dump(st, "nodes", NODES_COLS),
            dump(
                st,
                "declarations",
                "file,node,origin,short,subkind,located,summary",
            ),
            dump(st, "refs", "id,source,file,kind,target_name,origin"),
            dump(st, "file_contexts", "file,context"),
            dump(
                st,
                "enrichments",
                "path,content_hash,model,prompt_version,payload",
            ),
        ]
    }

    /// Requête de projection de la 0.20.0, copiée en dur.
    fn project_like_0_20(st: &Store) {
        let ids: Vec<String> = st.nodes().unwrap().into_iter().map(|n| n.id).collect();
        let mut up = st
            .conn
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
            .unwrap();
        for id in ids {
            up.execute([id]).unwrap();
        }
    }

    fn typed(q: &str, f: &str, sub: SubKind, sum: Option<&str>) -> Node {
        Node::new(
            NodeKind::Type,
            q,
            q.rsplit('.').next().unwrap(),
            Some(f),
            Some(sub),
            sum,
        )
    }

    fn many_files(n: usize) -> Vec<FileRecord> {
        (0..n).map(|i| file(&format!("d{i:02}.md"))).collect()
    }

    /// Structure du fichier `i` : deux types, une référence d'analyse avec contexte.
    fn item_of(i: usize) -> (String, Vec<Node>, Vec<Ref>) {
        let f = format!("d{i:02}.md");
        let a = typed(&format!("p.T{i}"), &f, SubKind::Class, Some("analyse"));
        let b = typed(&format!("p.U{i}"), &f, SubKind::Interface, None);
        let refs = vec![pref(&a.id, "X", Some(CTX)), pref(&b.id, "Y", Some(CTX))];
        (f, vec![a, b], refs)
    }

    /// Graphe de 50 fichiers dont 10 ont un enrichissement conservé (résumé, concept, mention).
    fn with_kept_enrichments(repo: &Path) -> Store {
        let mut st = Store::open_or_create(repo).unwrap();
        st.insert_pending_files(&many_files(50)).unwrap();
        let items: Vec<_> = (0..50).map(item_of).collect();
        st.replace_structures(&items).unwrap();
        for i in 0..10 {
            let (f, nodes, _) = item_of(i);
            st.apply_enrichment(
                &f,
                "h",
                "m",
                &[(nodes[1].id.clone(), format!("résumé {i}"))],
                &[concept(&format!("idée{i}"))],
                &[mention(&nodes[0].id, &format!("idée{i}"))],
            )
            .unwrap();
        }
        st
    }

    #[test]
    fn req064_ac1_fresh_base_writes_without_projection_and_matches_general_path() {
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let mut fresh = with_kept_enrichments(d1.path());
        let mut general = with_kept_enrichments(d2.path());
        let items: Vec<_> = (0..50).map(item_of).collect();
        fresh.reset_with_pending_files(&many_files(50)).unwrap();
        general.reset().unwrap();
        general.insert_pending_files(&many_files(50)).unwrap();
        reset_diagnostics();
        let mut reapplied = 0;
        for it in &items {
            reapplied += fresh
                .replace_structures_counted(std::slice::from_ref(it))
                .unwrap();
        }
        let d = diagnostics();
        assert_eq!(d.total_projections(), 0, "{d:?}");
        assert_eq!(reapplied, 10);
        let mut reapplied_general = 0;
        for it in &items {
            reapplied_general += general
                .replace_structures_counted(std::slice::from_ref(it))
                .unwrap();
        }
        assert_eq!(reapplied_general, 10);
        assert_eq!(graph_dump(&fresh), graph_dump(&general));
    }

    #[test]
    fn req064_ac1_fresh_base_without_shared_nodes_projects_nothing() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.reset_with_pending_files(&many_files(50)).unwrap();
        reset_diagnostics();
        let items: Vec<_> = (0..50).map(item_of).collect();
        st.replace_structures(&items).unwrap();
        assert_eq!(diagnostics().total_projections(), 0);
        assert_eq!(st.count_nodes().unwrap(), 100);
    }

    #[test]
    fn req064_ac2_shared_node_on_fresh_base_follows_the_smallest_path() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        let files: Vec<FileRecord> = ["a", "b", "c", "d1.md", "d2.md"]
            .iter()
            .map(|f| file(f))
            .collect();
        st.reset_with_pending_files(&files).unwrap();
        reset_diagnostics();
        let sh = |f: &str, short: &str, sub: SubKind, sum: Option<&str>| {
            Node::new(NodeKind::Type, "p.A", short, Some(f), Some(sub), sum)
        };
        st.replace_structure("c", &[sh("c", "Cc", SubKind::Enum, Some("sc"))], &[])
            .unwrap();
        st.replace_structure("a", &[sh("a", "Aa", SubKind::Class, None)], &[])
            .unwrap();
        st.replace_structure("b", &[sh("b", "Bb", SubKind::Interface, Some("sb"))], &[])
            .unwrap();
        st.replace_structure("d1.md", &[concept("idée")], &[])
            .unwrap();
        st.replace_structure("d2.md", &[concept("idée")], &[])
            .unwrap();
        let n = st.node_by_id("type:p.A").unwrap().unwrap();
        assert_eq!(n.short, "Aa");
        assert_eq!(n.subkind, Some(SubKind::Class));
        assert_eq!(n.file.as_deref(), Some("a"));
        assert_eq!(n.summary.as_deref(), Some("sb"));
        let summary_file: Option<String> = st
            .conn
            .query_row(
                "SELECT summary_file FROM nodes WHERE id = 'type:p.A'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(summary_file.as_deref(), Some("b"));
        let d = diagnostics();
        assert!(d.projections("type:p.A") <= 2, "{d:?}");
        assert!(d.projections("concept:idée") <= 1, "{d:?}");
        assert_eq!(st.node_by_id("concept:idée").unwrap().unwrap().file, None);
    }

    fn assert_no_double_projection(d: &Diagnostics, ids: &[String], max_total: usize) {
        for id in ids {
            assert!(
                d.projections(id) <= 1,
                "{id} projeté {} fois",
                d.projections(id)
            );
        }
        assert!(d.total_projections() as usize <= max_total, "{d:?}");
    }

    #[test]
    fn req064_ac3_general_path_projects_each_touched_node_once() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("x.md")]).unwrap();
        let mk = |range: std::ops::Range<usize>| -> Vec<Node> {
            range
                .map(|i| typed(&format!("p.N{i}"), "x.md", SubKind::Class, Some("a")))
                .collect()
        };
        let old = mk(0..30);
        st.replace_structure("x.md", &old, &[]).unwrap();
        let sums: Vec<(String, String)> = old
            .iter()
            .map(|n| (n.id.clone(), format!("r {}", n.short)))
            .collect();
        st.apply_enrichment("x.md", "h", "m", &sums, &[], &[])
            .unwrap();
        let all: Vec<String> = mk(0..40).into_iter().map(|n| n.id).collect();

        // Réécriture avec enrichissement conservé réappliqué : 30 avant, 30 après, 20 communs.
        reset_diagnostics();
        let n = st
            .replace_structures_counted(&[("x.md".to_string(), mk(10..40), vec![])])
            .unwrap();
        assert_eq!(n, 1);
        assert_no_double_projection(&diagnostics(), &all, 40);
        assert_eq!(st.count_nodes().unwrap(), 30);
        let kept = st.node_by_id("type:p.N15").unwrap().unwrap();
        assert_eq!(kept.summary.as_deref(), Some("r N15"));
        assert!(st.node_by_id("type:p.N5").unwrap().is_none());

        // Enrichissement.
        let new_nodes = mk(10..40);
        let sums: Vec<(String, String)> = new_nodes
            .iter()
            .map(|n| (n.id.clone(), "s".into()))
            .collect();
        reset_diagnostics();
        st.apply_enrichment("x.md", "h", "m", &sums, &[concept("c")], &[])
            .unwrap();
        let mut ids = all.clone();
        ids.push("concept:c".into());
        assert_no_double_projection(&diagnostics(), &ids, 31);

        // Suppression.
        reset_diagnostics();
        assert!(st.remove_file("x.md").unwrap());
        assert_no_double_projection(&diagnostics(), &ids, 31);
        assert_eq!(st.count_nodes().unwrap(), 0);
    }

    // Générateur congruentiel : séquences reproductibles.
    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    #[derive(Debug, Clone)]
    enum Op {
        Write(String, Vec<Node>, Vec<Ref>),
        Enrich(String, String, Vec<(String, String)>, Vec<Node>, Vec<Ref>),
        Remove(String),
        Add(String, String),
        Deps(Vec<Node>),
    }

    fn run_op(st: &mut Store, op: &Op) {
        match op {
            Op::Write(f, n, r) => st.replace_structure(f, n, r).unwrap(),
            Op::Enrich(f, h, s, c, r) => st.apply_enrichment(f, h, "m", s, c, r).unwrap(),
            Op::Remove(f) => {
                st.remove_file(f).unwrap();
            }
            Op::Add(f, h) => {
                let mut rec = file(f);
                rec.hash = h.clone();
                st.insert_files(&[rec]).unwrap();
            }
            Op::Deps(d) => st.replace_derived_dependencies(d).unwrap(),
        }
    }

    struct Gen {
        rng: Lcg,
        files: Vec<String>,
        hash: Vec<String>,
        known: Vec<bool>,
        declared: Vec<Vec<Node>>,
    }

    impl Gen {
        fn write(&mut self, i: usize) -> Op {
            let f = self.files[i].clone();
            let mut nodes: Vec<Node> = Vec::new();
            for _ in 0..self.rng.below(5) {
                let k = self.rng.below(6);
                let sub = [SubKind::Class, SubKind::Interface, SubKind::Enum][self.rng.below(3)];
                let sum = (self.rng.below(2) == 0).then(|| format!("p{i}{k}"));
                let n = if self.rng.below(5) == 0 {
                    Node::new(
                        NodeKind::Concept,
                        &format!("c{}", self.rng.below(3)),
                        "c",
                        None,
                        None,
                        sum.as_deref(),
                    )
                } else {
                    let q = format!("p.T{k}");
                    Node::new(
                        NodeKind::Type,
                        &q,
                        &format!("S{i}{k}"),
                        Some(&f),
                        Some(sub),
                        sum.as_deref(),
                    )
                };
                nodes.push(n);
            }
            let ctx = (self.rng.below(2) == 0).then_some(CTX);
            let mut refs = Vec::new();
            if !nodes.is_empty() {
                for _ in 0..self.rng.below(4) {
                    let s = &nodes[self.rng.below(nodes.len())];
                    refs.push(pref(&s.id, &format!("R{}", self.rng.below(4)), ctx));
                }
            }
            self.declared[i] = nodes.clone();
            Op::Write(f, nodes, refs)
        }

        fn enrich(&mut self, i: usize) -> Op {
            let f = self.files[i].clone();
            let decl = self.declared[i].clone();
            let mut sums: Vec<(String, String)> = Vec::new();
            for n in &decl {
                if self.rng.below(2) == 0 && !sums.iter().any(|(id, _)| *id == n.id) {
                    sums.push((n.id.clone(), format!("e{i}")));
                }
            }
            let mut concepts: Vec<Node> = Vec::new();
            for _ in 0..self.rng.below(3) {
                let name = format!("c{}", self.rng.below(4));
                let sum = (self.rng.below(2) == 0).then(|| format!("cs{i}"));
                let c = Node::new(NodeKind::Concept, &name, &name, None, None, sum.as_deref());
                if !concepts.iter().any(|x| x.id == c.id) {
                    concepts.push(c);
                }
            }
            let mut refs = Vec::new();
            if !decl.is_empty() {
                for c in &concepts {
                    if self.rng.below(2) == 0 {
                        refs.push(mention(&decl[self.rng.below(decl.len())].id, &c.qualified));
                    }
                }
                if self.rng.below(3) == 0 {
                    refs.push(mention(&decl[0].id, "inconnu"));
                }
            }
            Op::Enrich(f, self.hash[i].clone(), sums, concepts, refs)
        }

        fn random_op(&mut self) -> Op {
            let i = self.rng.below(self.files.len());
            match self.rng.below(7) {
                0 | 1 if self.known[i] => self.write(i),
                2 | 3 if self.known[i] && !self.declared[i].is_empty() => self.enrich(i),
                4 if self.known[i] => {
                    self.known[i] = false;
                    self.declared[i].clear();
                    Op::Remove(self.files[i].clone())
                }
                5 => {
                    self.known[i] = true;
                    self.hash[i] = ["h", "h2"][self.rng.below(2)].to_string();
                    Op::Add(self.files[i].clone(), self.hash[i].clone())
                }
                _ => Op::Deps(vec![Node::new(
                    NodeKind::Dependency,
                    &format!("g:{}", self.rng.below(3)),
                    "lib",
                    None,
                    None,
                    None,
                )]),
            }
        }
    }

    #[test]
    fn req064_ac4_random_sequences_leave_a_consistent_graph_identical_to_the_general_path() {
        for seed in 0..200u64 {
            let nfiles = 4;
            let mut g = Gen {
                rng: Lcg(seed * 7919 + 1),
                files: (0..nfiles).map(|i| format!("f{i}.md")).collect(),
                hash: vec!["h".to_string(); nfiles],
                known: vec![true; nfiles],
                declared: vec![Vec::new(); nfiles],
            };
            let records = |g: &Gen| -> Vec<FileRecord> {
                g.files
                    .iter()
                    .zip(&g.hash)
                    .map(|(f, h)| {
                        let mut r = file(f);
                        r.hash = h.clone();
                        r
                    })
                    .collect()
            };
            // Phase 1 : graphe peuplé avec enrichissements conservés (hash « h »).
            let d1 = tempdir().unwrap();
            let d2 = tempdir().unwrap();
            let mut fresh = Store::open_or_create(d1.path()).unwrap();
            let mut general = Store::open_or_create(d2.path()).unwrap();
            let mut prelude = Vec::new();
            for st in [&mut fresh, &mut general] {
                st.insert_pending_files(&records(&g)).unwrap();
            }
            for i in 0..nfiles {
                prelude.push(g.write(i));
                if !g.declared[i].is_empty() {
                    prelude.push(g.enrich(i));
                }
            }
            for op in &prelude {
                run_op(&mut fresh, op);
                run_op(&mut general, op);
            }
            // Phase 2 : graphe vidé (hash éventuellement changés), puis écritures.
            for i in 0..nfiles {
                g.hash[i] = ["h", "h2"][g.rng.below(2)].to_string();
                g.declared[i].clear();
            }
            fresh.reset_with_pending_files(&records(&g)).unwrap();
            general.reset().unwrap();
            general.insert_pending_files(&records(&g)).unwrap();
            let mut ops = Vec::new();
            for i in 0..nfiles {
                if g.rng.below(5) != 0 {
                    ops.push(g.write(i));
                }
            }
            for _ in 0..g.rng.below(7) {
                ops.push(g.random_op());
            }
            for (k, op) in ops.iter().enumerate() {
                run_op(&mut fresh, op);
                run_op(&mut general, op);
                assert_eq!(
                    graph_dump(&fresh),
                    graph_dump(&general),
                    "graine {seed}, après l'opération {k} : {op:?}"
                );
            }
            // (a) la projection de la 0.20.0 ne change aucune ligne.
            let before = dump(&fresh, "nodes", NODES_COLS);
            project_like_0_20(&fresh);
            assert_eq!(before, dump(&fresh, "nodes", NODES_COLS), "graine {seed}");
            // (b) aucun nœud sans déclaration hors dépendances dérivées.
            let orphans: i64 = fresh
                .conn
                .query_row(
                    "SELECT count(*) FROM nodes WHERE kind <> 'dependency'
                     AND NOT EXISTS (SELECT 1 FROM declarations WHERE node = nodes.id)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(orphans, 0, "graine {seed}");
        }
    }

    /// Tables touchées par une suppression de fichiers.
    fn removal_dump(st: &Store) -> Vec<Rows> {
        let mut v = graph_dump(st);
        v.push(dump(st, "files", "path,hash,state"));
        v.push(dump(st, "edges", "source,target,kind"));
        v.push(dump(st, "summary", "id,text"));
        v
    }

    #[test]
    fn req060_remove_files_matches_remove_file_one_by_one() {
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let mut grouped = with_kept_enrichments(d1.path());
        let mut single = with_kept_enrichments(d2.path());
        let gone = ["d02.md", "d05.md", "d11.md", "d40.md", "unknown.md"];
        reset_diagnostics();
        assert_eq!(grouped.remove_files(&gone).unwrap(), 4);
        assert_eq!(diagnostics().commits(), 1);
        for p in gone {
            single.remove_file(p).unwrap();
        }
        assert_eq!(removal_dump(&grouped), removal_dump(&single));
        assert_eq!(grouped.remove_files(&[]).unwrap(), 0);
        assert!(grouped.enrichment_entry("d02.md").unwrap().is_none());
    }

    #[test]
    fn req060_ac4_grouped_removal_is_all_or_nothing() {
        let dir = tempdir().unwrap();
        let mut st = with_kept_enrichments(dir.path());
        st.conn
            .execute_batch(
                "CREATE TRIGGER fail_third BEFORE DELETE ON files WHEN old.path = 'd04.md'
                 BEGIN SELECT RAISE(ABORT, 'test'); END;",
            )
            .unwrap();
        let before = removal_dump(&st);
        let err = st
            .remove_files(&["d02.md", "d03.md", "d04.md", "d05.md", "d06.md"])
            .unwrap_err();
        assert!(err.to_string().contains("test"), "{err}");
        assert_eq!(removal_dump(&st), before);
        assert_eq!(st.files().unwrap().len(), 50);
    }

    #[test]
    fn req060_apply_enrichments_matches_one_by_one_in_one_commit() {
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let mk = |repo: &Path| {
            let mut st = Store::open_or_create(repo).unwrap();
            st.insert_pending_files(&many_files(6)).unwrap();
            let items: Vec<_> = (0..6).map(item_of).collect();
            st.replace_structures(&items).unwrap();
            st
        };
        let writes: Vec<EnrichmentWrite> = (0..4)
            .map(|i| {
                let (f, nodes, _) = item_of(i);
                EnrichmentWrite {
                    path: f,
                    hash: "h".into(),
                    summaries: vec![(nodes[1].id.clone(), format!("résumé {i}"))],
                    concepts: vec![concept(&format!("idée{i}"))],
                    refs: vec![mention(&nodes[0].id, &format!("idée{i}"))],
                }
            })
            .collect();
        let mut grouped = mk(d1.path());
        let mut single = mk(d2.path());
        reset_diagnostics();
        grouped.apply_enrichments("m", &writes).unwrap();
        assert_eq!(diagnostics().commits(), 1);
        for w in &writes {
            single
                .apply_enrichment(&w.path, &w.hash, "m", &w.summaries, &w.concepts, &w.refs)
                .unwrap();
        }
        assert_eq!(graph_dump(&grouped), graph_dump(&single));
        // Un fichier inconnu annule tout le groupe.
        let mut bad = writes.clone();
        bad.push(EnrichmentWrite {
            path: "inconnu.md".into(),
            hash: "h".into(),
            summaries: vec![],
            concepts: vec![],
            refs: vec![],
        });
        let before = graph_dump(&grouped);
        let (f, nodes, _) = item_of(5);
        bad[0] = EnrichmentWrite {
            path: f,
            hash: "h2".into(),
            summaries: vec![(nodes[0].id.clone(), "autre".into())],
            concepts: vec![],
            refs: vec![],
        };
        assert!(grouped.apply_enrichments("m", &bad).is_err());
        assert_eq!(graph_dump(&grouped), before);
    }

    // --- REQ-069 : schéma 8, clé, tête et statut des références, marqueur ---

    fn marker(st: &Store) -> Option<String> {
        st.conn
            .query_row("SELECT value FROM meta WHERE key = 'resolution'", [], |r| {
                r.get(0)
            })
            .ok()
    }

    fn strings(st: &Store, sql: &str) -> Vec<String> {
        let mut q = st.conn.prepare(sql).unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn req069_ac1_fresh_graph_has_key_head_and_status_columns_and_indexes() {
        let dir = tempdir().unwrap();
        let st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(
            strings(
                &st,
                "SELECT name FROM pragma_table_info('refs') ORDER BY cid"
            ),
            [
                "id",
                "source",
                "file",
                "kind",
                "target_name",
                "origin",
                "target_key",
                "target_head",
                "unresolved"
            ]
        );
        for (name, cols, sql) in [
            (
                "idx_refs_key",
                "target_key",
                "CREATE INDEX idx_refs_key ON refs(target_key)",
            ),
            (
                "idx_refs_head",
                "target_head",
                "CREATE INDEX idx_refs_head ON refs(target_head) WHERE target_head IS NOT NULL",
            ),
            (
                "idx_refs_unresolved",
                "unresolved",
                "CREATE INDEX idx_refs_unresolved ON refs(unresolved) WHERE unresolved = 1",
            ),
        ] {
            assert_eq!(
                strings(
                    &st,
                    &format!("SELECT name FROM pragma_index_info('{name}') ORDER BY seqno")
                ),
                [cols],
                "{name}"
            );
            assert_eq!(
                strings(
                    &st,
                    &format!("SELECT sql FROM sqlite_master WHERE name = '{name}'")
                ),
                [sql]
            );
        }
        assert_eq!(st_user_version(dir.path()), 8);
        assert_eq!(st.schema_version().unwrap(), 8);
        assert_eq!(
            strings(&st, "SELECT value FROM meta WHERE key = 'schema_version'"),
            ["8"]
        );
    }

    const KEY_NAMES: [&str; 8] = [
        "a.b.C", "C", " C ", "T.m", "x/y#z", "g:a:1.0", "a.b/c.d", "",
    ];
    const KEYS: [&str; 8] = ["C", "C", "C", "m", "z", "0", "d", ""];
    const HEADS: [Option<&str>; 8] = [
        Some("a"),
        None,
        None,
        Some("T"),
        None,
        Some("g:a:1"),
        Some("a"),
        None,
    ];

    fn key_rows(st: &Store, origin: &str) -> Vec<(String, String, Option<String>)> {
        let mut q = st
            .conn
            .prepare("SELECT target_name, target_key, target_head FROM refs WHERE origin = ?1 ORDER BY id")
            .unwrap();
        q.query_map([origin], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn expected_keys() -> Vec<(String, String, Option<String>)> {
        (0..8)
            .map(|i| {
                (
                    KEY_NAMES[i].to_string(),
                    KEYS[i].to_string(),
                    HEADS[i].map(String::from),
                )
            })
            .collect()
    }

    #[test]
    fn req069_ac2_key_and_head_are_written_by_every_reference_path() {
        let refs_of = |src: &str| -> Vec<Ref> {
            KEY_NAMES
                .iter()
                .map(|n| Ref {
                    source: src.into(),
                    kind: EdgeKind::Imports,
                    target_name: (*n).into(),
                    context: None,
                })
                .collect()
        };
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        let a = ty("p.A", "a.java");
        // Base fraîche.
        st.reset_with_pending_files(&[file("a.java")]).unwrap();
        st.replace_structure("a.java", std::slice::from_ref(&a), &refs_of(&a.id))
            .unwrap();
        assert_eq!(key_rows(&st, "parse"), expected_keys());
        // Chemin général : le même fichier réécrit.
        st.replace_structure("a.java", std::slice::from_ref(&a), &refs_of(&a.id))
            .unwrap();
        assert_eq!(key_rows(&st, "parse"), expected_keys());
        // Enrichissement.
        st.apply_enrichment("a.java", "h", "m", &[], &[], &refs_of(&a.id))
            .unwrap();
        assert_eq!(key_rows(&st, "llm"), expected_keys());
        let flagged: i64 = st
            .conn
            .query_row("SELECT count(*) FROM refs WHERE unresolved <> 0", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(flagged, 0);
    }

    fn marked(st: &Store) -> Vec<(String, String, String)> {
        let mut q = st
            .conn
            .prepare("SELECT source, kind, target_name FROM refs WHERE unresolved = 1 ORDER BY source, kind, target_name")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn expected_unresolved(st: &Store) -> Vec<(String, String, String)> {
        let mut v: Vec<_> = crate::resolve::unresolved(st)
            .unwrap()
            .into_iter()
            .map(|r| (r.source, r.kind.as_str().to_string(), r.target_name))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn req069_ac3_status_and_marker_follow_the_full_pass() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        st.insert_pending_files(&[file("a.java"), file("b.md")])
            .unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        let (a, b) = (ty("p.A", "a.java"), ty("p.B", "a.java"));
        let doc = Node::new(NodeKind::Document, "b.md", "b.md", Some("b.md"), None, None);
        st.replace_structures(&[
            (
                "a.java".to_string(),
                vec![a.clone(), b.clone()],
                vec![
                    pref(&a.id, "p.B", None),
                    pref(&a.id, "Absent", None),
                    pref(&b.id, "Autre", None),
                    mention(&a.id, "texte libre"),
                ],
            ),
            ("b.md".to_string(), vec![doc.clone()], vec![]),
        ])
        .unwrap();
        st.apply_enrichment(
            "b.md",
            "h",
            "m",
            &[],
            &[concept("idée")],
            &[mention(&doc.id, "Inconnue"), mention(&doc.id, "idée")],
        )
        .unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        let stats = crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        let expected = expected_unresolved(&st);
        assert!(expected.len() >= 3, "{expected:?}");
        assert_eq!(stats.unresolved, expected.len());
        assert_eq!(marked(&st), expected);
        // Une deuxième passe complète remet à zéro avant de marquer (journal absent : la passe
        // n'est pas incrémentale, le statut a été modifié hors du stockage).
        st.log = None;
        st.conn
            .execute(
                "UPDATE refs SET unresolved = 1 WHERE target_name = 'p.B'",
                [],
            )
            .unwrap();
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marked(&st), expected);

        // Écritures qui remettent le marqueur à « à résoudre » ; passe qui le remet à « à jour ».
        st.insert_files(&[file("c.md")]).unwrap();
        st.set_enrich_model("m").unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        st.replace_structure("c.md", &[ty("p.C", "c.md")], &[])
            .unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        crate::resolve::resolve(&mut st).unwrap();
        st.apply_enrichments(
            "m",
            &[EnrichmentWrite {
                path: "b.md".into(),
                hash: "h".into(),
                summaries: vec![],
                concepts: vec![],
                refs: vec![],
            }],
        )
        .unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        st.remove_files(&["c.md"]).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marked(&st), expected_unresolved(&st));
        st.reset_with_pending_files(&[file("a.java")]).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        st.reset().unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
    }

    /// AC-066-4 : les requêtes de la passe incrémentale passent par des index ; seules exceptions,
    /// nommées : le chargement des nœuds de l'index de résolution et le compte des liens.
    #[test]
    fn req066_ac4_incremental_queries_use_indexes() {
        let dir = tempdir().unwrap();
        let st = Store::open_or_create(dir.path()).unwrap();
        let scans_nodes = ["RESOLVE_NODES"];
        let scans_edges = ["COUNT_EDGES", "SUMMARY_TOP_DEGREES"];
        for (name, sql) in INCREMENTAL_QUERIES {
            let mut stmt = st
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let params = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
            let details: Vec<String> = stmt
                .query_map(rusqlite::params_from_iter(params), |r| r.get(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(!details.is_empty() || name.starts_with("INSERT"), "{name}");
            let joined = details.join(" | ");
            assert_eq!(
                joined.contains("SCAN nodes"),
                scans_nodes.contains(&name),
                "{name} : {joined}"
            );
            assert_eq!(
                joined.contains("SCAN edges"),
                scans_edges.contains(&name),
                "{name} : {joined}"
            );
            assert!(!joined.contains("SCAN refs"), "{name} : {joined}");
            assert!(!joined.contains("SCAN declarations"), "{name} : {joined}");
        }
    }

    /// AC-069-3, passe incrémentale : le statut stocké et le marqueur suivent la passe.
    #[test]
    fn req069_ac3_status_and_marker_follow_the_incremental_pass() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.java"), file("b.md"), file("c.java")])
            .unwrap();
        let (a, b) = (ty("p.A", "a.java"), ty("p.B", "a.java"));
        let doc = Node::new(NodeKind::Document, "b.md", "b.md", Some("b.md"), None, None);
        st.replace_structures(&[
            (
                "a.java".to_string(),
                vec![a.clone(), b.clone()],
                vec![
                    pref(&a.id, "p.B", None),
                    pref(&a.id, "Absent", None),
                    pref(&b.id, "Later", None),
                    mention(&a.id, "texte libre"),
                ],
            ),
            (
                "b.md".to_string(),
                vec![doc.clone()],
                vec![mention(&doc.id, "Inconnue")],
            ),
        ])
        .unwrap();
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        assert_eq!(marked(&st).len(), 2);
        reset_diagnostics();

        // `Later` apparaît : sa référence se résout ; une nouvelle référence reste non résolue.
        let later = ty("q.Later", "c.java");
        st.replace_structure(
            "c.java",
            std::slice::from_ref(&later),
            &[pref(&later.id, "Nulle part", None)],
        )
        .unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        let stats = crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(diagnostics().incremental_passes(), 1);
        assert_eq!(diagnostics().full_passes(), 0);
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        let expected = expected_unresolved(&st);
        assert_eq!(expected.len(), 2, "{expected:?}");
        assert_eq!(stats.unresolved, expected.len());
        assert_eq!(marked(&st), expected);
        assert_eq!(st.count_unresolved().unwrap(), expected.len());

        // `Later` disparaît : sa référence redevient non résolue.
        st.remove_files(&["c.java"]).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(diagnostics().incremental_passes(), 2);
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        let expected = expected_unresolved(&st);
        assert_eq!(expected.len(), 2, "{expected:?}");
        assert_eq!(marked(&st), expected);

        // Écritures qui ne touchent ni nœuds ni références : le marqueur reste `à jour`.
        st.insert_files(&[file("d.md")]).unwrap();
        st.set_enrich_model("m").unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        st.apply_enrichments(
            "m",
            &[EnrichmentWrite {
                path: "b.md".into(),
                hash: "h".into(),
                summaries: vec![],
                concepts: vec![concept("idée")],
                refs: vec![mention(&doc.id, "idée"), mention(&doc.id, "Inconnue2")],
            }],
        )
        .unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(diagnostics().incremental_passes(), 3);
        let expected = expected_unresolved(&st);
        assert_eq!(expected.len(), 4, "{expected:?}");
        assert_eq!(marked(&st), expected);
        assert_eq!(st.count_unresolved().unwrap(), expected.len());
    }

    #[test]
    fn req069_marker_is_written_once_by_the_first_write_and_rolls_back_with_it() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        // Écriture refusée : le marqueur reste « à jour ».
        let a = ty("p.A", "a.md");
        let bad = Ref {
            kind: EdgeKind::Contains,
            ..pref(&a.id, "x", None)
        };
        assert!(st
            .replace_structure("a.md", std::slice::from_ref(&a), &[bad])
            .is_err());
        assert_eq!(marker(&st).as_deref(), Some("à jour"));
        st.replace_structure("a.md", &[a], &[]).unwrap();
        assert_eq!(marker(&st).as_deref(), Some("à résoudre"));
    }

    #[test]
    fn req069_ac4_schemas_7_6_and_5_are_migrated_with_key_head_and_marker() {
        let fresh_dir = tempdir().unwrap();
        let fresh_keys = {
            let st = rich(fresh_dir.path());
            dump(&st, "refs", "id,target_key,target_head,unresolved")
        };
        assert!(!fresh_keys.is_empty());
        let fresh_status = {
            let mut st = Store::open_existing(fresh_dir.path()).unwrap();
            crate::resolve::resolve(&mut st).unwrap();
            dump(&st, "refs", "id,unresolved")
        };
        for version in [7, 6, 5] {
            for existing in [false, true] {
                let (dir, mut st, before) = migrated_from(version, existing);
                let ctx = format!("depuis {version}, existing={existing}");
                assert_eq!(st.schema_version().unwrap(), SCHEMA_VERSION, "{ctx}");
                assert_eq!(st_user_version(dir.path()), SCHEMA_VERSION, "{ctx}");
                assert_eq!(snapshot7(&st), before, "{ctx}");
                assert_eq!(
                    dump(&st, "refs", "id,target_key,target_head,unresolved"),
                    fresh_keys,
                    "{ctx}"
                );
                assert_eq!(marker(&st).as_deref(), Some("à résoudre"), "{ctx}");
                crate::resolve::resolve(&mut st).unwrap();
                assert_eq!(dump(&st, "refs", "id,unresolved"), fresh_status, "{ctx}");
                assert_eq!(marker(&st).as_deref(), Some("à jour"), "{ctx}");
            }
        }
    }

    #[test]
    fn req069_ac4_migrated_keys_match_the_function() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        let a = ty("p.A", "a.java");
        st.insert_pending_files(&[file("a.java")]).unwrap();
        let refs: Vec<Ref> = KEY_NAMES.iter().map(|n| pref(&a.id, n, None)).collect();
        st.replace_structure("a.java", &[a], &refs).unwrap();
        drop(st);
        downgrade_to_7(dir.path());
        let st = Store::open_or_create(dir.path()).unwrap();
        assert_eq!(key_rows(&st, "parse"), expected_keys());
    }

    #[test]
    fn req066_ac7_a_contains_reference_is_refused_and_leaves_the_graph_unchanged() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.java"), file("b.java")])
            .unwrap();
        let (a, b) = (ty("p.A", "a.java"), ty("p.B", "b.java"));
        st.replace_structure(
            "b.java",
            std::slice::from_ref(&b),
            &[pref(&b.id, "A", Some(CTX))],
        )
        .unwrap();
        let contains = |src: &str| Ref {
            kind: EdgeKind::Contains,
            ..pref(src, "x", None)
        };
        let all = |st: &Store| {
            let mut v = graph_dump(st);
            v.push(dump(st, "edges", "source,target,kind"));
            v.push(dump(st, "meta", "key,value"));
            v
        };
        // Chemin général : le fichier b.java existe déjà.
        let before = all(&st);
        let e = st
            .replace_structure(
                "b.java",
                std::slice::from_ref(&b),
                &[pref(&b.id, "A", Some(CTX)), contains(&b.id)],
            )
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("référence de genre contains refusée dans b.java"),
            "{e}"
        );
        assert_eq!(all(&st), before);
        // Plusieurs fichiers en une transaction : rien n'est écrit pour a.java non plus.
        assert!(st
            .replace_structures(&[
                ("a.java".to_string(), vec![a.clone()], vec![]),
                ("b.java".to_string(), vec![b.clone()], vec![contains(&b.id)]),
            ])
            .is_err());
        assert_eq!(all(&st), before);
        // Base fraîche.
        st.reset_with_pending_files(&[file("a.java")]).unwrap();
        let fresh = all(&st);
        assert!(st
            .replace_structure("a.java", std::slice::from_ref(&a), &[contains(&a.id)])
            .is_err());
        assert_eq!(all(&st), fresh);
        // Enrichissement.
        st.replace_structure("a.java", std::slice::from_ref(&a), &[])
            .unwrap();
        let before = all(&st);
        let e = st
            .apply_enrichment("a.java", "h", "m", &[], &[], &[contains(&a.id)])
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("référence de genre contains refusée dans a.java"),
            "{e}"
        );
        assert_eq!(all(&st), before);
        assert!(st
            .apply_enrichments(
                "m",
                &[EnrichmentWrite {
                    path: "a.java".into(),
                    hash: "h".into(),
                    summaries: vec![],
                    concepts: vec![],
                    refs: vec![contains(&a.id)],
                }],
            )
            .is_err());
        assert_eq!(all(&st), before);
    }

    // --- REQ-066 : journal des changements et compteurs de lecture (DEC-077 point 1) ---

    type Ids = std::collections::BTreeSet<String>;

    fn ids(v: &[&str]) -> Ids {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn log_of(st: &Store) -> ChangeLog {
        st.change_log().expect("journal tenu").clone()
    }

    fn expected(
        inserted: &[&str],
        deleted: &[&str],
        changed: &[&str],
        files: &[&str],
        sources: &[&str],
    ) -> ChangeLog {
        ChangeLog {
            inserted: ids(inserted),
            deleted: ids(deleted),
            declarations_changed: ids(changed),
            files: ids(files),
            removed_sources: ids(sources),
        }
    }

    fn settle(st: &mut Store) {
        crate::resolve::resolve(st).unwrap();
        assert_eq!(log_of(st), ChangeLog::default());
    }

    fn jref(src: &Node, target: &str) -> Ref {
        pref(&src.id, target, Some(CTX))
    }

    /// a.java : A, B, S (partagé) ; c.java : S, C. Graphe résolu, journal vide.
    fn shared_graph(repo: &Path) -> Store {
        let mut st = Store::open_or_create(repo).unwrap();
        st.insert_pending_files(&[file("a.java"), file("c.java")])
            .unwrap();
        let (a, b, s) = (
            ty("p.A", "a.java"),
            ty("p.B", "a.java"),
            ty("p.S", "a.java"),
        );
        let (s2, c) = (ty("p.S", "c.java"), ty("p.C", "c.java"));
        st.replace_structure(
            "a.java",
            &[a.clone(), b.clone(), s],
            &[jref(&a, "p.C"), jref(&b, "p.S")],
        )
        .unwrap();
        st.replace_structure("c.java", &[s2, c.clone()], &[jref(&c, "p.A")])
            .unwrap();
        settle(&mut st);
        st
    }

    #[test]
    fn req066_journal_is_none_or_empty_according_to_the_marker() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        assert!(st.change_log().is_none());
        st.insert_pending_files(&[file("a.java")]).unwrap();
        st.reset_with_pending_files(&[file("a.java")]).unwrap();
        assert!(st.change_log().is_none());
        // Base fraîche : le journal n'est pas tenu.
        let a = ty("p.A", "a.java");
        st.replace_structure("a.java", std::slice::from_ref(&a), &[jref(&a, "x")])
            .unwrap();
        assert!(st.change_log().is_none());
        drop(st);
        // Graphe `à résoudre` à l'ouverture.
        let mut st = Store::open_existing(dir.path()).unwrap();
        assert!(st.change_log().is_none());
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(log_of(&st), ChangeLog::default());
        st.replace_structure("a.java", std::slice::from_ref(&a), &[])
            .unwrap();
        assert!(!log_of(&st).is_empty());
        drop(st);
        // Écriture sans passe : le marqueur est `à résoudre`.
        let mut st = Store::open_existing(dir.path()).unwrap();
        assert!(st.change_log().is_none());
        crate::resolve::resolve(&mut st).unwrap();
        drop(st);
        // Graphe `à jour` à l'ouverture : journal vide.
        let mut st = Store::open_existing(dir.path()).unwrap();
        assert_eq!(st.change_log(), Some(&ChangeLog::default()));
        st.reset().unwrap();
        assert!(st.change_log().is_none());
        // Un graphe créé est `à résoudre`.
        let other = tempdir().unwrap();
        let st = Store::open_or_create(other.path()).unwrap();
        assert!(st.change_log().is_none());
    }

    #[test]
    fn req066_journal_of_a_new_file() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        st.insert_pending_files(&[file("d.java")]).unwrap();
        assert_eq!(log_of(&st), ChangeLog::default());
        let d = ty("p.D", "d.java");
        st.replace_structure("d.java", std::slice::from_ref(&d), &[jref(&d, "p.A")])
            .unwrap();
        assert_eq!(
            log_of(&st),
            expected(&["type:p.D"], &[], &["type:p.D"], &["d.java"], &[])
        );
        // Sans référence, le fichier n'est pas dans le journal.
        st.insert_pending_files(&[file("e.java")]).unwrap();
        st.replace_structure("e.java", &[ty("p.E", "e.java")], &[])
            .unwrap();
        let log = log_of(&st);
        assert!(log.inserted.contains("type:p.E"));
        assert!(!log.files.contains("e.java"));
    }

    #[test]
    fn req066_journal_of_a_modified_file() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        let (a, s) = (ty("p.A", "a.java"), ty("p.S", "a.java"));
        let d = ty("p.D", "a.java");
        // B retiré, D ajouté, A et S redéclarés à l'identique, références réécrites.
        st.replace_structure(
            "a.java",
            &[a.clone(), s, d],
            &[jref(&a, "p.C"), jref(&a, "p.D")],
        )
        .unwrap();
        assert_eq!(
            log_of(&st),
            expected(
                &["type:p.D"],
                &["type:p.B"],
                &["type:p.B", "type:p.D"],
                &["a.java"],
                &["type:p.A", "type:p.B"]
            )
        );
    }

    #[test]
    fn req066_journal_shared_node_kept_but_declarations_changed() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        let (a, b) = (ty("p.A", "a.java"), ty("p.B", "a.java"));
        st.replace_structure("a.java", &[a.clone(), b.clone()], &[jref(&a, "p.C")])
            .unwrap();
        // S n'est plus déclaré par a.java mais l'est encore par c.java : conservé.
        assert_eq!(
            log_of(&st),
            expected(
                &[],
                &[],
                &["type:p.S"],
                &["a.java"],
                &["type:p.A", "type:p.B"]
            )
        );
        assert!(st.node_by_id("type:p.S").unwrap().is_some());
    }

    #[test]
    fn req066_journal_ignores_a_summary_only_change() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        st.apply_enrichment(
            "a.java",
            "h",
            "m",
            &[("type:p.A".into(), "résumé".into())],
            &[],
            &[],
        )
        .unwrap();
        assert!(log_of(&st).is_empty(), "{:?}", log_of(&st));
        // Même chose en réécrivant la structure : seules les références y figurent.
        let (a, b, s) = (
            ty("p.A", "a.java"),
            ty("p.B", "a.java"),
            ty("p.S", "a.java"),
        );
        st.replace_structure("a.java", &[a.clone(), b.clone(), s], &[jref(&a, "p.C")])
            .unwrap();
        let log = log_of(&st);
        assert!(log.declarations_changed.is_empty() && log.inserted.is_empty());
        assert!(log.deleted.is_empty());
    }

    #[test]
    fn req066_journal_of_enrichments() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("a.md")]).unwrap();
        let d = ty("p.D", "a.md");
        st.replace_structure("a.md", std::slice::from_ref(&d), &[])
            .unwrap();
        settle(&mut st);
        let write = |st: &mut Store, concepts: &[&str], refs: &[(&str, &str)]| {
            st.apply_enrichments(
                "m",
                &[EnrichmentWrite {
                    path: "a.md".into(),
                    hash: "h".into(),
                    summaries: vec![],
                    concepts: concepts.iter().map(|c| concept(c)).collect(),
                    refs: refs.iter().map(|(s, t)| mention(s, t)).collect(),
                }],
            )
            .unwrap();
        };
        // Concept ajouté.
        write(&mut st, &["c1"], &[("type:p.D", "c1")]);
        assert_eq!(
            log_of(&st),
            expected(&["concept:c1"], &[], &["concept:c1"], &["a.md"], &[])
        );
        settle(&mut st);
        // Concept remplacé : c1 retiré, c2 ajouté, références `llm` remplacées.
        write(&mut st, &["c2"], &[("type:p.D", "c2")]);
        assert_eq!(
            log_of(&st),
            expected(
                &["concept:c2"],
                &["concept:c1"],
                &["concept:c1", "concept:c2"],
                &["a.md"],
                &["type:p.D"]
            )
        );
        settle(&mut st);
        // Concept conservé, référence `llm` remplacée seule.
        write(
            &mut st,
            &["c2"],
            &[("type:p.D", "c2"), ("type:p.D", "c2 bis")],
        );
        assert_eq!(
            log_of(&st),
            expected(&[], &[], &[], &["a.md"], &["type:p.D"])
        );
        settle(&mut st);
        // Concept retiré, plus aucune référence.
        write(&mut st, &[], &[]);
        assert_eq!(
            log_of(&st),
            expected(
                &[],
                &["concept:c2"],
                &["concept:c2"],
                &["a.md"],
                &["type:p.D"]
            )
        );
        // Un enregistrement vide ne change rien.
        settle(&mut st);
        write(&mut st, &[], &[]);
        assert!(log_of(&st).is_empty());
    }

    /// AC-088-3 : `needs_resolution()` suit le marqueur `meta.resolution`.
    #[test]
    fn req088_ac3_needs_resolution_follows_the_marker() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        assert!(!st.needs_resolution().unwrap(), "après une passe");
        st.insert_pending_files(&[file("d.java")]).unwrap();
        st.replace_structure("d.java", &[ty("p.D", "d.java")], &[])
            .unwrap();
        assert!(st.needs_resolution().unwrap(), "après replace_structure");
        settle(&mut st);
        assert!(!st.needs_resolution().unwrap());
        st.apply_enrichments(
            "m",
            &[EnrichmentWrite {
                path: "a.java".into(),
                hash: "h".into(),
                summaries: vec![],
                concepts: vec![concept("idée")],
                refs: vec![],
            }],
        )
        .unwrap();
        assert!(st.needs_resolution().unwrap(), "après apply_enrichments");
        settle(&mut st);
        assert!(!st.needs_resolution().unwrap());
        st.remove_files(&["c.java"]).unwrap();
        assert!(st.needs_resolution().unwrap(), "après remove_files");
        settle(&mut st);
        assert!(!st.needs_resolution().unwrap());
        st.reset_with_pending_files(&[file("z.java")]).unwrap();
        assert!(st.needs_resolution().unwrap(), "après le vidage d'un build");
        settle(&mut st);
        drop(st);

        // Graphe rouvert : la valeur de la clé décide, en écriture comme en lecture seule.
        let db = || Connection::open(db_path(dir.path())).unwrap();
        let hash = || blake3::hash(&std::fs::read(db_path(dir.path())).unwrap());
        let check = |expected: bool, what: &str| {
            let before = hash();
            assert_eq!(
                Store::open_readonly(dir.path())
                    .unwrap()
                    .needs_resolution()
                    .unwrap(),
                expected,
                "lecture seule : {what}"
            );
            assert_eq!(hash(), before, "{what} : graph.db inchangé");
            assert_eq!(
                Store::open_existing(dir.path())
                    .unwrap()
                    .needs_resolution()
                    .unwrap(),
                expected,
                "écriture : {what}"
            );
        };
        check(false, "à jour");
        db().execute(
            "UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
            [],
        )
        .unwrap();
        check(true, "à résoudre");
        db().execute(
            "UPDATE meta SET value = 'à jour' WHERE key = 'resolution'",
            [],
        )
        .unwrap();
        check(false, "à jour de nouveau");
        // Clé absente (schéma 7) : à résoudre, lu sans migration ni écriture.
        downgrade_to_7(dir.path());
        let before = hash();
        let ro = Store::open_readonly(dir.path()).unwrap();
        assert_eq!(ro.opened_schema_version(), 7);
        assert!(ro.needs_resolution().unwrap(), "clé absente");
        drop(ro);
        assert_eq!(hash(), before);
    }

    #[test]
    fn req066_journal_of_removed_files() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        st.remove_files(&["a.java"]).unwrap();
        // A et B orphelins supprimés, S partagé conservé.
        assert_eq!(
            log_of(&st),
            expected(
                &[],
                &["type:p.A", "type:p.B"],
                &["type:p.A", "type:p.B", "type:p.S"],
                &["a.java"],
                &["type:p.A", "type:p.B"]
            )
        );
        assert!(st.node_by_id("type:p.S").unwrap().is_some());
        st.remove_files(&["c.java", "inconnu.java"]).unwrap();
        let log = log_of(&st);
        assert!(log.deleted.contains("type:p.S") && log.deleted.contains("type:p.C"));
        assert!(log.files.contains("c.java") && !log.files.contains("inconnu.java"));
        assert!(log.removed_sources.contains("type:p.C"));
    }

    #[test]
    fn req066_failed_transaction_leaves_the_journal_unchanged() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        let d = ty("p.D", "a.java");
        st.insert_pending_files(&[file("d.java")]).unwrap();
        st.replace_structure("d.java", &[ty("p.D", "d.java")], &[])
            .unwrap();
        let before = log_of(&st);
        st.conn
            .execute_batch(
                "CREATE TRIGGER fail_c BEFORE DELETE ON files WHEN old.path = 'c.java'
                 BEGIN SELECT RAISE(ABORT, 'test'); END;",
            )
            .unwrap();
        assert!(st.remove_files(&["a.java", "c.java"]).is_err());
        assert_eq!(log_of(&st), before);
        // Référence refusée : rien n'est journalisé non plus.
        let bad = Ref {
            kind: EdgeKind::Contains,
            ..pref(&d.id, "x", None)
        };
        assert!(st
            .replace_structure("d.java", &[ty("p.D", "d.java")], &[bad])
            .is_err());
        assert_eq!(log_of(&st), before);
    }

    #[test]
    fn req066_dependency_nodes_of_a_pass_are_in_the_next_journal() {
        let dir = tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        st.insert_pending_files(&[file("pom.xml")]).unwrap();
        let module = |q: &str| Node::new(NodeKind::Module, q, "m", Some("pom.xml"), None, None);
        let m = module("com.acme:m");
        let dep = |t: &str| Ref {
            kind: EdgeKind::DependsOn,
            ..pref(&m.id, t, None)
        };
        st.replace_structure("pom.xml", std::slice::from_ref(&m), &[dep("org.x:x")])
            .unwrap();
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(
            log_of(&st),
            expected(&["dependency:org.x:x"], &[], &[], &[], &[])
        );
        // Dépendance remplacée : x supprimé, y créé ; le journal de la passe suivante les nomme.
        st.replace_structure("pom.xml", std::slice::from_ref(&m), &[dep("org.y:y")])
            .unwrap();
        crate::resolve::resolve(&mut st).unwrap();
        assert_eq!(
            log_of(&st),
            expected(
                &["dependency:org.y:y"],
                &["dependency:org.x:x"],
                &[],
                &[],
                &[]
            )
        );
        // Passe sans changement : journal vide.
        crate::resolve::resolve(&mut st).unwrap();
        assert!(log_of(&st).is_empty());
    }

    #[test]
    fn req066_read_counters_count_node_and_edge_rows() {
        let dir = tempdir().unwrap();
        let mut st = shared_graph(dir.path());
        // Quatre nœuds (A, B, S, C) et trois liens : A→C, B→S, C→A.
        assert_eq!(st.count_nodes().unwrap(), 4);
        let n_edges = st.edges().unwrap().len();
        assert_eq!(n_edges, 3);
        reset_diagnostics();
        assert_eq!(diagnostics().node_rows_read(), 0);
        assert_eq!(diagnostics().edge_rows_read(), 0);
        st.nodes().unwrap();
        assert_eq!(diagnostics().node_rows_read(), 4);
        st.node_by_id("type:p.A").unwrap();
        st.node_by_id("type:absent").unwrap();
        assert_eq!(diagnostics().node_rows_read(), 5);
        let found = st.nodes_matching("p.A").unwrap();
        // Chaque requête de résolution compte ses lignes rendues.
        // `p.A` : une seule ligne, par `RESOLVE_QUALIFIED` (ni identifiant, ni nom court, ni suffixe
        // à frontière ne correspondent) ; 5 lignes déjà lues.
        assert_eq!(found.len(), 1);
        assert_eq!(diagnostics().node_rows_read(), 5 + 1);
        reset_diagnostics();
        st.edges().unwrap();
        assert_eq!(diagnostics().edge_rows_read(), 3);
        st.edges_at("type:p.A", true).unwrap();
        st.edges_at("type:p.A", false).unwrap();
        assert_eq!(diagnostics().edge_rows_read(), 5);
        assert_eq!(diagnostics().node_rows_read(), 0);
        // Les agrégats ne comptent pas.
        st.count_edges().unwrap();
        st.count_nodes().unwrap();
        st.count_unresolved().unwrap();
        assert_eq!(diagnostics().edge_rows_read(), 5);
        // La lecture du diff de `replace_edges` compte les liens stockés.
        reset_diagnostics();
        st.replace_edges(&[]).unwrap();
        assert_eq!(diagnostics().edge_rows_read(), 3);
        // Passes et références réévaluées.
        reset_diagnostics();
        note_pass(false);
        note_pass(true);
        note_pass(true);
        note_refs_reevaluated(7);
        let d = diagnostics();
        assert_eq!((d.full_passes(), d.incremental_passes()), (1, 2));
        assert_eq!(d.refs_reevaluated(), 7);
        reset_diagnostics();
        assert_eq!(diagnostics(), Diagnostics::default());
        // La passe complète lit les nœuds et les liens de la passe (journal absent : les liens ont
        // été supprimés hors du stockage, une passe incrémentale ne les rétablirait pas).
        st.log = None;
        crate::resolve::resolve(&mut st).unwrap();
        let d = diagnostics();
        // Nœuds : 4 (`nodes()` de la passe, le résumé n'en relit aucun) + 0 (aucun nœud
        // `dependency` lu par `replace_dependencies_tx`). Liens : 0 (diff de `replace_edges_tx` :
        // `replace_edges(&[])` les a tous supprimés ; le résumé de la passe complète est calculé
        // sur les liens de la passe, DEC-078).
        assert_eq!((d.node_rows_read(), d.edge_rows_read()), (4, 0));
    }

    #[test]
    fn req085_ac1_read_only_store_survives_a_migration_done_meanwhile() {
        for version in [6, 5, 7] {
            let dir = tempdir().unwrap();
            drop(rich(dir.path()));
            match version {
                6 => downgrade_to_6(dir.path()),
                5 => downgrade_to_5(dir.path()),
                _ => downgrade_to_7(dir.path()),
            }
            let reference = tempdir().unwrap();
            std::fs::create_dir_all(db_path(reference.path()).parent().unwrap()).unwrap();
            std::fs::copy(db_path(dir.path()), db_path(reference.path())).unwrap();

            let reader = Store::open_readonly(dir.path()).unwrap();
            assert_eq!(reader.opened_schema_version(), version);
            drop(Store::open_existing(dir.path()).unwrap());
            assert_eq!(
                st_user_version(dir.path()),
                SCHEMA_VERSION,
                "depuis {version}"
            );

            let base = Store::open_readonly(reference.path()).unwrap();
            assert_eq!(base.opened_schema_version(), version);
            assert_eq!(
                reader.file_contexts().unwrap(),
                base.file_contexts().unwrap(),
                "schéma {version}"
            );
            assert_eq!(
                reader.refs_with_origin().unwrap(),
                base.refs_with_origin().unwrap(),
                "schéma {version}"
            );
            let (a, b) = (
                crate::resolve::unresolved(&reader).unwrap(),
                crate::resolve::unresolved(&base).unwrap(),
            );
            assert!(!b.is_empty());
            assert_eq!(a, b, "schéma {version}");
            assert_eq!(reader.nodes().unwrap(), base.nodes().unwrap());
            assert_eq!(reader.edges().unwrap(), base.edges().unwrap());
            assert!(!base.files().unwrap().is_empty());
            assert_eq!(reader.files().unwrap(), base.files().unwrap());
            drop(reader);
            let again = Store::open_readonly(dir.path()).unwrap();
            assert_eq!(again.opened_schema_version(), SCHEMA_VERSION);
        }
    }

    #[test]
    fn req085_ac2_read_only_store_keeps_its_snapshot() {
        let dir = tempdir().unwrap();
        drop(rich(dir.path()));
        assert!(!Store::open_or_create(dir.path())
            .unwrap()
            .in_read_snapshot());
        let mut writer = Store::open_existing(dir.path()).unwrap();
        assert!(!writer.in_read_snapshot());

        let reader = Store::open_readonly(dir.path()).unwrap();
        assert!(reader.in_read_snapshot());
        let (nodes, edges, files) = (
            reader.nodes().unwrap(),
            reader.edges().unwrap(),
            reader.files().unwrap(),
        );
        let started = std::time::Instant::now();
        assert_eq!(writer.remove_files(&["B.java"]).unwrap(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        assert_eq!(reader.nodes().unwrap(), nodes);
        assert_eq!(reader.edges().unwrap(), edges);
        assert_eq!(reader.files().unwrap(), files);
        assert!(reader.in_read_snapshot());
        drop(reader);
        let after = Store::open_readonly(dir.path()).unwrap();
        assert!(after.files().unwrap().len() < files.len());
        assert!(after.nodes().unwrap().len() < nodes.len());
    }

    // REQ-098, AC-098-5 : vérification seule, sans suivre de lien.
    #[cfg(unix)]
    mod symlink_guard {
        use super::*;
        use std::os::unix::fs::symlink;

        fn msg(r: Result<()>) -> String {
            match r {
                Err(Error::Message(m)) => m,
                other => panic!("erreur attendue, obtenu {other:?}"),
            }
        }

        fn expected(rel: &str) -> String {
            format!("{rel} est un lien symbolique ; supprimez-le")
        }

        #[test]
        fn req098_ac5_absent_ou_reel_est_accepte() {
            let repo = tempdir().unwrap();
            ensure_no_symlink(repo.path()).unwrap();
            std::fs::create_dir(repo.path().join(".nephila")).unwrap();
            ensure_no_symlink(repo.path()).unwrap();
            std::fs::write(repo.path().join(".nephila/graph.db"), "x").unwrap();
            std::fs::write(repo.path().join(".nephila/lock"), "").unwrap();
            ensure_no_symlink(repo.path()).unwrap();
        }

        #[test]
        fn req098_ac5_nephila_lien_nomme_nephila_seul() {
            let repo = tempdir().unwrap();
            let victim = tempdir().unwrap();
            symlink(victim.path().join("graph.db"), victim.path().join("lock")).unwrap();
            symlink(victim.path().join("x"), victim.path().join("graph.db")).unwrap();
            symlink(victim.path(), repo.path().join(".nephila")).unwrap();
            assert_eq!(msg(ensure_no_symlink(repo.path())), expected(".nephila"));
        }

        #[test]
        fn req098_ac5_nephila_lien_vers_absent() {
            let repo = tempdir().unwrap();
            symlink("/nonexistent-nephila-target", repo.path().join(".nephila")).unwrap();
            assert_eq!(msg(ensure_no_symlink(repo.path())), expected(".nephila"));
        }

        #[test]
        fn req098_ac5_chaque_fichier_en_lien_est_nomme() {
            for name in [
                "graph.db",
                "lock",
                "graph.db-wal",
                "graph.db-shm",
                "graph.db-journal",
            ] {
                let repo = tempdir().unwrap();
                std::fs::create_dir(repo.path().join(".nephila")).unwrap();
                symlink(
                    "/nonexistent-nephila-target",
                    repo.path().join(".nephila").join(name),
                )
                .unwrap();
                assert_eq!(
                    msg(ensure_no_symlink(repo.path())),
                    expected(&format!(".nephila/{name}"))
                );
            }
        }

        #[test]
        fn req098_ac5_ordre_fixe_graph_db_avant_lock() {
            let repo = tempdir().unwrap();
            std::fs::create_dir(repo.path().join(".nephila")).unwrap();
            symlink("/nonexistent-a", repo.path().join(".nephila/lock")).unwrap();
            symlink("/nonexistent-b", repo.path().join(".nephila/graph.db")).unwrap();
            assert_eq!(
                msg(ensure_no_symlink(repo.path())),
                expected(".nephila/graph.db")
            );
        }

        #[test]
        fn req098_ac5_les_trois_ouvertures_et_le_verrou_refusent() {
            let repo = tempdir().unwrap();
            let victim = tempdir().unwrap();
            symlink(victim.path(), repo.path().join(".nephila")).unwrap();
            let want = expected(".nephila");
            assert_eq!(msg(Store::open_or_create(repo.path()).map(|_| ())), want);
            assert_eq!(msg(Store::open_existing(repo.path()).map(|_| ())), want);
            assert_eq!(msg(Store::open_readonly(repo.path()).map(|_| ())), want);
            assert_eq!(
                msg(crate::index::lock::RepoLock::acquire(repo.path()).map(|_| ())),
                want
            );
            assert_eq!(std::fs::read_dir(victim.path()).unwrap().count(), 0);
        }
    }
}
