//! Assistant de test « schéma 10 » (DEC-173), en SQL écrit en dur : ramène un graphe de schéma 11
//! au schéma 10 (table `enrich_calls` supprimée, versions 10).

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA10_DOWNGRADE: &str = "BEGIN;
    DROP TABLE enrich_calls;
    UPDATE meta SET value = '10' WHERE key = 'schema_version';
    PRAGMA user_version = 10;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 11 au schéma 10.
pub fn downgrade_to_10(db: &Connection) {
    db.execute_batch(SCHEMA10_DOWNGRADE).unwrap();
}
