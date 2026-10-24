//! Assistant de test « schéma 6 de la 0.20.0 » (DEC-073 point 6), en SQL écrit en dur : ramène un
//! graphe de schéma 8 au schéma 7 (assistant `schema7`), puis au schéma 6 (`refs` recréée avec `AUTOINCREMENT` et `context` rempli depuis
//! `file_contexts`, index de la 0.20.0, `file_contexts` et `idx_declarations_node_file`
//! supprimés, versions 6). Copie de celui du module de tests du stockage.

use rusqlite::Connection;

#[path = "schema7.rs"]
mod schema7;

pub const SCHEMA6_DOWNGRADE: &str = "BEGIN;
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

/// Ramène le graphe ouvert de schéma 8 au schéma 6 de la 0.20.0 (le schéma 7 d'abord).
pub fn downgrade_to_6(db: &Connection) {
    schema7::downgrade_to_7(db);
    db.execute_batch(SCHEMA6_DOWNGRADE).unwrap();
}

/// Ramène le graphe ouvert de schéma 8 au schéma 5 (6, puis 6 -> 5).
pub fn downgrade_to_5(db: &Connection) {
    downgrade_to_6(db);
    db.execute_batch(
        "DROP INDEX idx_nodes_qualified_rev; ALTER TABLE nodes DROP COLUMN qualified_rev;
         UPDATE meta SET value = '5' WHERE key = 'schema_version'; PRAGMA user_version = 5;",
    )
    .unwrap();
}
