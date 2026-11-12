//! Assistant de test « schéma 15 » (DEC-217), en SQL écrit en dur : ramène un graphe de schéma 17
//! au schéma 15 (colonne `enrichments.nodes_hash` supprimée, index plein texte `nodes_fts` et ses trois déclencheurs supprimés, versions 15).

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA15_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE enrichments DROP COLUMN nodes_hash;
    DROP TRIGGER nodes_fts_ai;
    DROP TRIGGER nodes_fts_ad;
    DROP TRIGGER nodes_fts_au;
    DROP TABLE nodes_fts;
    UPDATE meta SET value = '15' WHERE key = 'schema_version';
    PRAGMA user_version = 15;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 17 au schéma 15.
pub fn downgrade_to_15(db: &Connection) {
    db.execute_batch(SCHEMA15_DOWNGRADE).unwrap();
}
