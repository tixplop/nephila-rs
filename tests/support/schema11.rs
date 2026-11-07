//! Assistant de test « schéma 11 » (DEC-180), en SQL écrit en dur : ramène un graphe de schéma 13
//! au schéma 11 (schéma 12 d'abord ; colonne `declarations.doc` supprimée, versions 11).

#![allow(dead_code)]

use rusqlite::Connection;

#[path = "schema12.rs"]
pub mod schema12;

pub const SCHEMA11_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE declarations DROP COLUMN doc;
    UPDATE meta SET value = '11' WHERE key = 'schema_version';
    PRAGMA user_version = 11;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 13 au schéma 11 (le schéma 12 d'abord).
pub fn downgrade_to_11(db: &Connection) {
    schema12::downgrade_to_12(db);
    db.execute_batch(SCHEMA11_DOWNGRADE).unwrap();
}
