//! Assistant de test « schéma 7 de la 0.21.0 » (DEC-076 point 6), en SQL écrit en dur : ramène un
//! graphe de schéma 8 au schéma 7 (index nouveaux supprimés, trois colonnes de `refs` retirées,
//! clé `meta.resolution` supprimée, versions 7). Copie de celui du module de tests du stockage.

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA7_DOWNGRADE: &str = "BEGIN;
    DROP INDEX idx_refs_key;
    DROP INDEX idx_refs_head;
    DROP INDEX idx_refs_unresolved;
    ALTER TABLE refs DROP COLUMN target_key;
    ALTER TABLE refs DROP COLUMN target_head;
    ALTER TABLE refs DROP COLUMN unresolved;
    DELETE FROM meta WHERE key = 'resolution';
    UPDATE meta SET value = '7' WHERE key = 'schema_version';
    PRAGMA user_version = 7;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 8 au schéma 7 de la 0.21.0.
pub fn downgrade_to_7(db: &Connection) {
    db.execute_batch(SCHEMA7_DOWNGRADE).unwrap();
}
