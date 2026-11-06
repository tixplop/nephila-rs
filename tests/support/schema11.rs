//! Assistant de test « schéma 11 » (DEC-180), en SQL écrit en dur : ramène un graphe de schéma 12
//! au schéma 11 (colonne `declarations.doc` supprimée, versions 11).

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA11_DOWNGRADE: &str = "BEGIN;
    ALTER TABLE declarations DROP COLUMN doc;
    UPDATE meta SET value = '11' WHERE key = 'schema_version';
    PRAGMA user_version = 11;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 12 au schéma 11.
pub fn downgrade_to_11(db: &Connection) {
    db.execute_batch(SCHEMA11_DOWNGRADE).unwrap();
}
