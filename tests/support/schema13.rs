//! Assistant de test « schéma 13 » (DEC-196), en SQL écrit en dur : ramène un graphe de schéma 15
//! au schéma 14 (assistant `schema14`) puis au schéma 13 (table `subsystem_names` et colonne `communities.description` supprimées,
//! versions 13).

#![allow(dead_code)]

use rusqlite::Connection;

#[path = "schema14.rs"]
pub mod schema14;

pub const SCHEMA13_DOWNGRADE: &str = "BEGIN;
    DROP TABLE subsystem_names;
    ALTER TABLE communities DROP COLUMN description;
    UPDATE meta SET value = '13' WHERE key = 'schema_version';
    PRAGMA user_version = 13;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 15 au schéma 13 (le schéma 14 d'abord).
pub fn downgrade_to_13(db: &Connection) {
    schema14::downgrade_to_14(db);
    db.execute_batch(SCHEMA13_DOWNGRADE).unwrap();
}
