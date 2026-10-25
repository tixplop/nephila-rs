//! Verrou d'écriture exclusif sur `<dépôt>/.nephila/lock` (REQ-020, DEC-012).
//!
//! Le verrou est un `flock(LOCK_EX | LOCK_NB)` : le noyau le libère à la mort du processus,
//! un fichier resté sur disque sans détenteur ne bloque donc personne.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::error::{Error, Result};

/// Verrou tenu tant que la valeur existe.
#[derive(Debug)]
pub struct RepoLock {
    _file: File,
}

impl RepoLock {
    /// Prend le verrou sans attendre ; échoue immédiatement s'il est déjà tenu.
    pub fn acquire(repo: &Path) -> Result<RepoLock> {
        crate::store::ensure_no_symlink(repo)?;
        let dir = repo.join(".nephila");
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Message(format!("impossible de créer {} : {e}", dir.display())))?;
        let path = dir.join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| {
                Error::Message(format!(
                    "impossible d'ouvrir le verrou {} : {e}",
                    path.display()
                ))
            })?;
        // SAFETY: le descripteur est valide pendant toute la durée de l'appel.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            return Err(if e.kind() == std::io::ErrorKind::WouldBlock {
                Error::Message("indexation déjà en cours sur ce dépôt".to_string())
            } else {
                Error::Message(format!("verrou {} impossible : {e}", path.display()))
            });
        }
        Ok(RepoLock { _file: file })
    }
}
