use crate::paths::RuntimePaths;
use crate::protocol::{self, Request, Response};
use crate::runtime::Runtime;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    ConfigureSocket(io::Error),
    RegisterSignal(io::Error),
    InitializeRuntime(crate::runtime::Error),
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
            Self::ConfigureSocket(error) => {
                write!(formatter, "cannot configure control socket: {error}")
            }
            Self::RegisterSignal(error) => {
                write!(formatter, "cannot register shutdown signal: {error}")
            }
            Self::InitializeRuntime(error) => {
                write!(formatter, "cannot initialize runtime: {error}")
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

    let paths = RuntimePaths::new(prefix.clone());
    paths.prepare().map_err(Error::PreparePaths)?;
    let daemon_lock = DaemonLock::acquire(paths.lock_path())?;
    let control_socket = ControlSocket::bind(paths.socket_path(), &daemon_lock)?;
    let mut runtime = Runtime::initialize(paths).map_err(Error::InitializeRuntime)?;
    control_socket
        .listener
        .set_nonblocking(true)
        .map_err(Error::ConfigureSocket)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))
        .map_err(Error::RegisterSignal)?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))
        .map_err(Error::RegisterSignal)?;

    println!(
        "termux-stacks {} daemon ready (installation: {}, prefix: {})",
        env!("CARGO_PKG_VERSION"),
        runtime.installation_id(),
        prefix.display()
    );

    control_socket.serve(&mut runtime, &shutdown)
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

        match try_lock_exclusive(&file) {
            Ok(true) => Ok(Self { _file: file }),
            Ok(false) => Err(Error::AlreadyRunning),
            Err(error) => Err(Error::LockDaemon(error)),
        }
    }
}

fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    loop {
        // SAFETY: `file` owns a valid descriptor for the duration of the call,
        // and `flock` neither retains the descriptor nor dereferences pointers.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(true);
        }

        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Ok(false),
            _ => return Err(error),
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

    fn serve(&self, runtime: &mut Runtime, shutdown: &AtomicBool) -> Result<(), Error> {
        while !shutdown.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((mut stream, _address)) => {
                    if let Err(error) = stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .and_then(|()| {
                            stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))
                        })
                    {
                        eprintln!("termux-stacks daemon: cannot bound client socket: {error}");
                        continue;
                    }
                    if let Err(error) = handle_connection(&mut stream, runtime) {
                        eprintln!("termux-stacks daemon: client request failed: {error}");
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::ServeSocket(error)),
            }
        }
        runtime.shutdown();
        Ok(())
    }
}

fn handle_connection(
    stream: &mut UnixStream,
    runtime: &mut Runtime,
) -> Result<(), protocol::ProtocolError> {
    let request = {
        let mut reader = BufReader::new(&mut *stream);
        match protocol::read_request(&mut reader) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                let response = Response::failure("", "protocol_error", error.to_string());
                protocol::write_frame(stream, &response)?;
                return Ok(());
            }
        }
    };

    let request_id = request.request_id().to_owned();
    let response = match request.validate_envelope() {
        Ok(()) => dispatch(request, runtime),
        Err(error) => Response::failure(request_id, "protocol_error", error.to_string()),
    };
    if let Err(error) = runtime.cache_response(&response) {
        eprintln!(
            "termux-stacks daemon: could not cache response for {:?}: {error}",
            response.request_id
        );
    }
    protocol::write_frame(stream, &response)
}

fn dispatch(request: Request, runtime: &mut Runtime) -> Response {
    match request {
        Request::Status {
            request_id, stack, ..
        } => {
            if let Err(error) = crate::manifest::validate_stack_name(&stack) {
                return Response::failure(request_id, error.kind().code(), error.to_string());
            }
            match runtime.status(&stack) {
                Ok(Some(status)) => Response::success(
                    request_id,
                    serde_json::to_value(status).expect("StackStatus is serializable"),
                ),
                Ok(None) => Response::success(
                    request_id,
                    serde_json::json!({"name": stack, "observed_state": "absent"}),
                ),
                Err(error) => Response::failure(request_id, error.code(), error.to_string()),
            }
        }
        Request::Up {
            request_id,
            manifest,
            ..
        } => match crate::manifest::parse(&manifest) {
            Ok(parsed) => match runtime.replay_response(&request_id, "up", &parsed.name) {
                Ok(Some(response)) => response,
                Ok(None) => match runtime.up(&request_id, &manifest, &parsed) {
                    Ok(status) => Response::success(
                        request_id,
                        serde_json::to_value(status).expect("StackStatus is serializable"),
                    ),
                    Err(error) => Response::failure(request_id, error.code(), error.to_string()),
                },
                Err(error) => Response::failure(request_id, error.code(), error.to_string()),
            },
            Err(error) => Response::failure(request_id, error.kind().code(), error.to_string()),
        },
        Request::Down {
            request_id, stack, ..
        } => {
            if let Err(error) = crate::manifest::validate_stack_name(&stack) {
                return Response::failure(request_id, error.kind().code(), error.to_string());
            }
            match runtime.replay_response(&request_id, "down", &stack) {
                Ok(Some(response)) => response,
                Ok(None) => match runtime.down(&request_id, &stack) {
                    Ok(status) => Response::success(
                        request_id,
                        serde_json::to_value(status).expect("StackStatus is serializable"),
                    ),
                    Err(error) => Response::failure(request_id, error.code(), error.to_string()),
                },
                Err(error) => Response::failure(request_id, error.code(), error.to_string()),
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
        std::thread::sleep(std::time::Duration::from_millis(20));

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
