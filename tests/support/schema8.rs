//! Assistant de test « schéma 8 de la 0.24.4 » (DEC-120), en SQL écrit en dur : ramène un graphe
//! de schéma 9 au schéma 8 (tables `node_communities` et `communities` supprimées, versions 8).
//! Copie de celui du module de tests du stockage.

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA8_DOWNGRADE: &str = "BEGIN;
    DROP TABLE node_communities;
    DROP TABLE communities;
    UPDATE meta SET value = '8' WHERE key = 'schema_version';
    PRAGMA user_version = 8;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 9 au schéma 8 de la 0.24.4.
pub fn downgrade_to_8(db: &Connection) {
    db.execute_batch(SCHEMA8_DOWNGRADE).unwrap();
}
