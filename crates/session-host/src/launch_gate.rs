//! One host serializes launches, so a short private socket name suffices. Its
//! path must fit wherever host.sock fits; UUID filenames exceed Unix sun_path
//! even in otherwise valid state directories.
use anyhow::{Result, ensure};
use std::{
    fs,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};

pub struct LaunchGate {
    pub listener: UnixListener,
    pub path: PathBuf,
    identity: (u64, u64),
}
impl LaunchGate {
    /// Caller holds the host's exclusive state-directory lock. An old socket
    /// may remain after SIGKILL; unrelated regular files/symlinks are preserved.
    pub fn bind(root: &Path) -> Result<Self> {
        let path = root.join("g.sock");
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::getuid() },
                    "unexpected launch gate path; preserved {}",
                    path.display()
                );
                fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        let gate = Self {
            listener,
            path,
            identity: (metadata.dev(), metadata.ino()),
        };
        fs::set_permissions(&gate.path, fs::Permissions::from_mode(0o600))?;
        gate.listener.set_nonblocking(true)?;
        Ok(gate)
    }
}
impl Drop for LaunchGate {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket() && (metadata.dev(), metadata.ino()) == self.identity
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_socket_is_replaced_and_only_owned_inode_is_removed() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("g.sock");
        drop(UnixListener::bind(&path)?);
        let gate = LaunchGate::bind(directory.path())?;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        fs::remove_file(&path)?;
        let replacement = UnixListener::bind(&path)?;
        drop(gate);
        assert!(path.exists());
        drop(replacement);
        fs::remove_file(&path)?;
        let gate = LaunchGate::bind(directory.path())?;
        drop(gate);
        assert!(!path.exists());
        Ok(())
    }
    #[test]
    fn unexpected_file_is_preserved() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("g.sock");
        fs::write(&path, b"original")?;
        assert!(LaunchGate::bind(directory.path()).is_err());
        assert_eq!(fs::read(&path)?, b"original");
        Ok(())
    }
}
