//! Assistant de test « schéma 9 » (DEC-138), en SQL écrit en dur : ramène un graphe de schéma 11
//! (schéma 10 d'abord) au schéma 9 (colonne `enrichments.language` et clé `meta.enrich_language` supprimées, versions 9). Copie de celui du module de
//! tests du stockage.

#![allow(dead_code)]

use rusqlite::Connection;

#[path = "schema10.rs"]
pub mod schema10;

pub const SCHEMA9_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE enrichments DROP COLUMN language;
    DELETE FROM meta WHERE key = 'enrich_language';
    UPDATE meta SET value = '9' WHERE key = 'schema_version';
    PRAGMA user_version = 9;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 11 au schéma 9 (le schéma 10 d'abord).
pub fn downgrade_to_9(db: &Connection) {
    schema10::downgrade_to_10(db);
    db.execute_batch(SCHEMA9_DOWNGRADE).unwrap();
}
