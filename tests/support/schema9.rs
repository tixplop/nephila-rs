//! Assistant de test « schéma 9 » (DEC-138), en SQL écrit en dur : ramène un graphe de schéma 10
//! au schéma 9 (colonne `enrichments.language` et clé `meta.enrich_language` supprimées, versions 9). Copie de celui du module de
//! tests du stockage.

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA9_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE enrichments DROP COLUMN language;
    DELETE FROM meta WHERE key = 'enrich_language';
    UPDATE meta SET value = '9' WHERE key = 'schema_version';
    PRAGMA user_version = 9;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 10 au schéma 9.
pub fn downgrade_to_9(db: &Connection) {
    db.execute_batch(SCHEMA9_DOWNGRADE).unwrap();
}
