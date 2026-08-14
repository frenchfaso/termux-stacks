use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RuntimePaths {
    prefix: PathBuf,
    state_dir: PathBuf,
    logs_dir: PathBuf,
    run_dir: PathBuf,
    lock_path: PathBuf,
    socket_path: PathBuf,
}

impl RuntimePaths {
    pub(crate) fn new(prefix: PathBuf) -> Self {
        let state_dir = prefix.join("var/lib/termux-stacks");
        let run_dir = prefix.join("var/run/termux-stacks");
        let logs_dir = state_dir.join("logs");
        let lock_path = run_dir.join("daemon.lock");
        let socket_path = run_dir.join("daemon.sock");
        Self {
            prefix,
            state_dir,
            logs_dir,
            run_dir,
            lock_path,
            socket_path,
        }
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub(crate) fn database_path(&self) -> PathBuf {
        self.state_dir.join("state.db")
    }

    pub(crate) fn prepare_stack_log_directory(&self, stack: &str) -> io::Result<()> {
        ensure_private_subtree(&self.state_dir, &["logs", stack])?;
        Ok(())
    }

    pub(crate) fn prepare_volume_directory(
        &self,
        stack: &str,
        volume: &str,
    ) -> io::Result<PathBuf> {
        ensure_private_subtree(&self.state_dir, &["volumes", stack, volume])?;
        Ok(self.volume_path(stack, volume))
    }

    pub(crate) fn volume_path(&self, stack: &str, volume: &str) -> PathBuf {
        self.state_dir.join("volumes").join(stack).join(volume)
    }

    pub(crate) fn log_paths(&self, stack: &str, service: &str) -> (PathBuf, PathBuf) {
        (
            self.logs_dir
                .join(stack)
                .join(format!("{service}.stdout.log")),
            self.logs_dir
                .join(stack)
                .join(format!("{service}.stderr.log")),
        )
    }

    pub(crate) fn prepare(&self) -> io::Result<()> {
        ensure_directory_tree(&self.prefix, &["var", "lib", "termux-stacks"])?;
        ensure_directory_tree(&self.prefix, &["var", "run", "termux-stacks"])?;
        verify_private_directory(&self.state_dir)?;
        verify_private_directory(&self.run_dir)
    }
}

fn ensure_private_subtree(base: &Path, components: &[&str]) -> io::Result<()> {
    verify_private_directory(base)?;
    let mut current = base.to_path_buf();
    for component in components {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(invalid_path(
                    &current,
                    "private path is not a real directory",
                ));
            }
            Ok(_) => verify_private_directory(&current)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700).create(&current)?;
                verify_private_directory(&current)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn ensure_directory_tree(prefix: &Path, components: &[&str]) -> io::Result<()> {
    let prefix_metadata = fs::symlink_metadata(prefix)?;
    if prefix_metadata.file_type().is_symlink() || !prefix_metadata.is_dir() {
        return Err(invalid_path(prefix, "prefix is not a real directory"));
    }

    let mut current = prefix.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid_path(&current, "symbolic links are not allowed"));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(invalid_path(&current, "path component is not a directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                if index + 1 == components.len() {
                    builder.mode(0o700);
                }
                builder.create(&current)?;
                let metadata = fs::symlink_metadata(&current)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(invalid_path(
                        &current,
                        "directory changed while it was being created",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn verify_private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_path(path, "private path is not a real directory"));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(invalid_path(path, "private directory mode must be 0700"));
    }

    Ok(())
}

fn invalid_path(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{}: {reason}", path.display()),
    )
}

#[cfg(test)]
pub(crate) fn test_prefix(label: &str) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let name = format!("txs-{label}-{}-{unique}", std::process::id());
    let mut prefix = std::env::temp_dir().join(&name);
    if prefix
        .join("var/run/termux-stacks/daemon.sock")
        .as_os_str()
        .as_bytes()
        .len()
        > 90
    {
        #[cfg(target_os = "android")]
        {
            let compact = format!("txs-{:x}-{unique:x}", std::process::id());
            prefix = std::env::temp_dir().join(compact);
        }
        #[cfg(target_os = "macos")]
        let short_temp = Path::new("/private/tmp");
        #[cfg(all(not(target_os = "macos"), not(target_os = "android")))]
        let short_temp = Path::new("/tmp");
        #[cfg(not(target_os = "android"))]
        {
            prefix = short_temp.join(name);
        }
    }
    fs::create_dir(&prefix).expect("create test prefix");
    prefix
}

#[cfg(test)]
mod tests {
    use super::{RuntimePaths, test_prefix};
    use std::fs;

    #[test]
    fn prepares_private_state_and_run_directories() {
        let prefix = test_prefix("path");
        let paths = RuntimePaths::new(prefix.clone());

        paths.prepare().expect("prepare paths");

        assert!(prefix.join("var/lib/termux-stacks").is_dir());
        assert!(prefix.join("var/run/termux-stacks").is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let state_mode = fs::metadata(prefix.join("var/lib/termux-stacks"))
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777;
            let run_mode = fs::metadata(prefix.join("var/run/termux-stacks"))
                .expect("run metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(state_mode, 0o700);
            assert_eq!(run_mode, 0o700);
        }

        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_in_managed_path() {
        use std::os::unix::fs::symlink;

        let prefix = test_prefix("symlink");
        let outside = test_prefix("outside");
        fs::create_dir(prefix.join("var")).expect("create var");
        symlink(&outside, prefix.join("var/lib")).expect("create symlink");

        let paths = RuntimePaths::new(prefix.clone());
        let error = paths.prepare().expect_err("symlink must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

        fs::remove_dir_all(prefix).expect("remove test prefix");
        fs::remove_dir_all(outside).expect("remove outside prefix");
    }

    #[test]
    fn creates_private_persistent_volume_directories() {
        use std::os::unix::fs::PermissionsExt;

        let prefix = test_prefix("volume");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");

        let volume = paths
            .prepare_volume_directory("hello", "data")
            .expect("prepare volume");
        assert_eq!(volume, paths.volume_path("hello", "data"));
        assert!(volume.is_dir());
        assert_eq!(
            fs::metadata(&volume)
                .expect("volume metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        fs::remove_dir_all(prefix).expect("remove test prefix");
    }
}
