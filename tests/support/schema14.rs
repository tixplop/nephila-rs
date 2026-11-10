//! Assistant de test « schéma 14 » (DEC-208), en SQL écrit en dur : ramène un graphe de schéma 15
//! au schéma 14 (colonne `enrichments.served_model` supprimée, versions 14).

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA14_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE enrichments DROP COLUMN served_model;
    UPDATE meta SET value = '14' WHERE key = 'schema_version';
    PRAGMA user_version = 14;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 15 au schéma 14.
pub fn downgrade_to_14(db: &Connection) {
    db.execute_batch(SCHEMA14_DOWNGRADE).unwrap();
}
