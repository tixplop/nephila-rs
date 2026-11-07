//! Assistant de test « schéma 12 » (DEC-190), en SQL écrit en dur : ramène un graphe de schéma 13
//! au schéma 12 (une seule entrée d'enrichissement par chemin, celle de plus grand `used_at`,
//! sans colonne `used_at` ; versions 12).

#![allow(dead_code)]

use rusqlite::Connection;

pub const SCHEMA12_DOWNGRADE: &str = "BEGIN;
    CREATE TABLE enrichments_12 (
        path           TEXT PRIMARY KEY,
        content_hash   TEXT NOT NULL,
        model          TEXT NOT NULL,
        prompt_version INTEGER NOT NULL,
        payload        TEXT NOT NULL,
        language       TEXT NOT NULL
    );
    INSERT INTO enrichments_12(path, content_hash, model, prompt_version, payload, language)
        SELECT e.path, e.content_hash, e.model, e.prompt_version, e.payload, e.language
        FROM enrichments e
        WHERE e.rowid = (SELECT x.rowid FROM enrichments x WHERE x.path = e.path
                         ORDER BY x.used_at DESC, x.rowid DESC LIMIT 1);
    DROP TABLE enrichments;
    ALTER TABLE enrichments_12 RENAME TO enrichments;
    UPDATE meta SET value = '12' WHERE key = 'schema_version';
    PRAGMA user_version = 12;
    COMMIT;";

/// Ramène le graphe ouvert de schéma 13 au schéma 12.
pub fn downgrade_to_12(db: &Connection) {
    db.execute_batch(SCHEMA12_DOWNGRADE).unwrap();
}
