//! Indexation du dépôt : commandes `build` (REQ-009) et `update` (REQ-011).

pub mod build;
pub mod enrich;
pub mod lock;
pub mod structure;
pub mod update;

/// Plafond de `--jobs` (REQ-071) : une faute de frappe ne doit pas créer des milliers de fils.
pub const MAX_JOBS: usize = 256;

/// Nombre de fils par défaut (REQ-071, DEC-083) : cœurs disponibles pour le processus, au plus
/// `MAX_JOBS` ; 4 si le système ne le donne pas. Seule définition du défaut.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(MAX_JOBS))
        .unwrap_or(4)
}
