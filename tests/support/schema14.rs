//! Assistant de test « schéma 14 » (DEC-208), en SQL écrit en dur : ramène un graphe de schéma 16
//! au schéma 15 (assistant `schema15`) puis au schéma 14 (colonne `enrichments.served_model` supprimée, versions 14).

#![allow(dead_code)]

use rusqlite::Connection;

#[path = "schema15.rs"]
pub mod schema15;

pub const SCHEMA14_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE enrichments DROP COLUMN served_model;
    UPDATE meta SET value = '14' WHERE key = 'schema_version';
    PRAGMA user_version = 14;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 16 au schéma 14 (le schéma 15 d'abord).
pub fn downgrade_to_14(db: &Connection) {
    schema15::downgrade_to_15(db);
    db.execute_batch(SCHEMA14_DOWNGRADE).unwrap();
}
