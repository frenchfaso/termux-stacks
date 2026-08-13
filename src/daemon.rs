use crate::paths::RuntimePaths;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) enum Error {
    MissingPrefix,
    RelativePrefix(PathBuf),
    PreparePaths(io::Error),
    AlreadyRunning,
    UnsafeLockPath(PathBuf),
    OpenLock(io::Error),
    LockDaemon(io::Error),
    UnsafeSocketPath(PathBuf),
    BindSocket(io::Error),
    SecureSocket(io::Error),
    ServeSocket(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPrefix => write!(
                formatter,
                "PREFIX is not set; run the daemon inside a Termux environment"
            ),
            Self::RelativePrefix(path) => {
                write!(formatter, "PREFIX must be absolute, got {}", path.display())
            }
            Self::PreparePaths(error) => write!(formatter, "cannot prepare runtime paths: {error}"),
            Self::AlreadyRunning => write!(formatter, "another daemon is already running"),
            Self::UnsafeLockPath(path) => {
                write!(formatter, "unsafe daemon lock path {}", path.display())
            }
            Self::OpenLock(error) => write!(formatter, "cannot open daemon lock: {error}"),
            Self::LockDaemon(error) => write!(formatter, "cannot lock daemon state: {error}"),
            Self::UnsafeSocketPath(path) => write!(
                formatter,
                "refusing to replace non-socket path {}",
                path.display()
            ),
            Self::BindSocket(error) => write!(formatter, "cannot bind control socket: {error}"),
            Self::SecureSocket(error) => {
                write!(formatter, "cannot set control socket permissions: {error}")
            }
            Self::ServeSocket(error) => write!(formatter, "control socket failed: {error}"),
        }
    }
}

pub(crate) fn run() -> Result<(), Error> {
    let prefix = std::env::var_os("PREFIX").ok_or(Error::MissingPrefix)?;
    let prefix = PathBuf::from(prefix);
    if !prefix.is_absolute() {
        return Err(Error::RelativePrefix(prefix));
    }

    let paths = RuntimePaths::new(prefix);
    paths.prepare().map_err(Error::PreparePaths)?;
    let daemon_lock = DaemonLock::acquire(paths.lock_path())?;
    let control_socket = ControlSocket::bind(paths.socket_path(), &daemon_lock)?;

    println!(
        "termux-stacks {} daemon scaffold is idle; control API is not implemented (prefix: {})",
        env!("CARGO_PKG_VERSION"),
        paths.prefix().display()
    );

    // Accepting and closing connections keeps the singleton probe reliable
    // without pretending that the S5 control protocol already exists. S0
    // deliberately relies on the operating system's default signal
    // termination; signal-aware draining follows the proot-distro spike.
    control_socket.serve_scaffold()
}

struct DaemonLock {
    _file: File,
}

impl DaemonLock {
    fn acquire(path: &Path) -> Result<Self, Error> {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::UnsafeLockPath(path.to_path_buf()));
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(Error::OpenLock)?;

        let file_metadata = file.metadata().map_err(Error::OpenLock)?;
        let path_metadata = fs::symlink_metadata(path).map_err(Error::OpenLock)?;
        if !file_metadata.is_file()
            || path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || file_metadata.dev() != path_metadata.dev()
            || file_metadata.ino() != path_metadata.ino()
            || path_metadata.permissions().mode() & 0o077 != 0
        {
            return Err(Error::UnsafeLockPath(path.to_path_buf()));
        }

        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(Error::AlreadyRunning),
            Err(TryLockError::Error(error)) => Err(Error::LockDaemon(error)),
        }
    }
}

struct ControlSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl ControlSocket {
    fn bind(path: &Path, _daemon_lock: &DaemonLock) -> Result<Self, Error> {
        match UnixListener::bind(path) {
            Ok(listener) => Self::finish(listener, path),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                if UnixStream::connect(path).is_ok() {
                    return Err(Error::AlreadyRunning);
                }

                let metadata = fs::symlink_metadata(path).map_err(Error::BindSocket)?;
                if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
                    return Err(Error::UnsafeSocketPath(path.to_path_buf()));
                }

                fs::remove_file(path).map_err(Error::BindSocket)?;
                let listener = UnixListener::bind(path).map_err(Error::BindSocket)?;
                Self::finish(listener, path)
            }
            Err(error) => Err(Error::BindSocket(error)),
        }
    }

    fn finish(listener: UnixListener, path: &Path) -> Result<Self, Error> {
        let before = fs::symlink_metadata(path).map_err(Error::SecureSocket)?;
        if before.file_type().is_symlink() || !before.file_type().is_socket() {
            return Err(Error::UnsafeSocketPath(path.to_path_buf()));
        }

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(Error::SecureSocket)?;

        let after = fs::symlink_metadata(path).map_err(Error::SecureSocket)?;
        if after.file_type().is_symlink()
            || !after.file_type().is_socket()
            || before.dev() != after.dev()
            || before.ino() != after.ino()
        {
            return Err(Error::UnsafeSocketPath(path.to_path_buf()));
        }

        Ok(Self {
            listener,
            path: path.to_path_buf(),
            device: after.dev(),
            inode: after.ino(),
        })
    }

    fn serve_scaffold(&self) -> Result<(), Error> {
        loop {
            match self.listener.accept() {
                Ok((stream, _address)) => drop(stream),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::ServeSocket(error)),
            }
        }
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlSocket, DaemonLock, Error};
    use crate::paths::RuntimePaths;
    use std::fs;

    #[test]
    fn control_socket_is_a_singleton_and_recovers_a_stale_socket() {
        let prefix = crate::paths::test_prefix("control-socket");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let daemon_lock = DaemonLock::acquire(paths.lock_path()).expect("acquire daemon lock");

        let first =
            ControlSocket::bind(paths.socket_path(), &daemon_lock).expect("first socket bind");
        assert!(matches!(
            ControlSocket::bind(paths.socket_path(), &daemon_lock),
            Err(Error::AlreadyRunning)
        ));

        drop(first);
        let stale_path = paths.socket_path().to_path_buf();
        let stale_listener =
            std::os::unix::net::UnixListener::bind(&stale_path).expect("make stale socket");
        drop(stale_listener);

        let recovered =
            ControlSocket::bind(paths.socket_path(), &daemon_lock).expect("recover stale socket");
        drop(recovered);
        drop(daemon_lock);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn daemon_lock_is_exclusive_and_is_released_on_close() {
        let prefix = crate::paths::test_prefix("daemon-lock");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");

        let first = DaemonLock::acquire(paths.lock_path()).expect("first daemon lock");
        assert!(matches!(
            DaemonLock::acquire(paths.lock_path()),
            Err(Error::AlreadyRunning)
        ));

        drop(first);
        let second = DaemonLock::acquire(paths.lock_path()).expect("lock after close");
        drop(second);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }
}
