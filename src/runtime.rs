use crate::engine::{self, Engine};
use crate::manifest::Manifest;
use crate::paths::RuntimePaths;
use crate::store::{StackStatus, Store};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

const SESSION_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Runtime {
    paths: RuntimePaths,
    store: Store,
    engine: Engine,
    children: BTreeMap<String, ManagedChild>,
}

struct ManagedChild {
    child: Child,
    starttime: u64,
    boot_id: String,
    session_id: Option<u32>,
}

#[derive(Debug)]
pub(crate) enum Error {
    Store(crate::store::Error),
    Engine(engine::Error),
    Io(io::Error),
    UnsafeLog(PathBufDisplay),
    ChildExited(Option<i32>),
    SessionUnknown,
    IdentityChanged,
    StopTimeout,
    ProtocolState(String),
    Unsupported(String),
}

#[derive(Debug)]
pub(crate) struct PathBufDisplay(std::path::PathBuf);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "{error}"),
            Self::Engine(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "runtime I/O failed: {error}"),
            Self::UnsafeLog(path) => write!(formatter, "unsafe log path {}", path.0.display()),
            Self::ChildExited(code) => {
                write!(
                    formatter,
                    "workload exited during startup with code {code:?}"
                )
            }
            Self::SessionUnknown => formatter.write_str(
                "the engine did not publish a qualified session; the workload state is unknown",
            ),
            Self::IdentityChanged => {
                formatter.write_str("persisted workload identity no longer matches the owned child")
            }
            Self::StopTimeout => {
                formatter.write_str("owned workload did not exit after engine kill")
            }
            Self::ProtocolState(message) => formatter.write_str(message),
            Self::Unsupported(message) => formatter.write_str(message),
        }
    }
}

impl Error {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Store(crate::store::Error::Conflict(_)) => "conflict",
            Self::Store(crate::store::Error::NotFound(_)) => "not_found",
            Self::Store(_) => "state_store",
            Self::Engine(_) => "engine",
            Self::Io(_) | Self::UnsafeLog(_) => "io",
            Self::ChildExited(_) => "start_failed",
            Self::SessionUnknown | Self::IdentityChanged | Self::StopTimeout => "unknown",
            Self::ProtocolState(_) => "state_store",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

impl From<crate::store::Error> for Error {
    fn from(error: crate::store::Error) -> Self {
        Self::Store(error)
    }
}

impl From<engine::Error> for Error {
    fn from(error: engine::Error) -> Self {
        Self::Engine(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl Runtime {
    pub(crate) fn initialize(paths: RuntimePaths) -> Result<Self, Error> {
        let store = Store::open(&paths.database_path())?;
        let engine = Engine::discover()?;
        engine.probe()?;
        let reconciled = store.reconcile_cold_start()?;
        if reconciled > 0 {
            eprintln!(
                "termux-stacks daemon: marked {reconciled} service(s) unknown after cold start"
            );
        }
        Ok(Self {
            paths,
            store,
            engine,
            children: BTreeMap::new(),
        })
    }

    pub(crate) fn installation_id(&self) -> &str {
        self.store.installation_id()
    }

    pub(crate) fn replay_response(
        &self,
        request_id: &str,
        operation: &str,
        stack: &str,
    ) -> Result<Option<crate::protocol::Response>, Error> {
        let Some(replay) = self.store.operation_replay(request_id)? else {
            return Ok(None);
        };
        if replay.operation != operation || replay.stack_name != stack {
            return Ok(Some(crate::protocol::Response::failure(
                request_id,
                "request_id_conflict",
                "request_id was already used for a different operation",
            )));
        }
        match replay.response_json {
            Some(json) => serde_json::from_str(&json).map(Some).map_err(|error| {
                Error::ProtocolState(format!("persisted response is invalid: {error}"))
            }),
            None => Ok(Some(crate::protocol::Response::failure(
                request_id,
                "unknown",
                "a previous attempt with this request_id has no durable response; no effect was retried",
            ))),
        }
    }

    pub(crate) fn cache_response(&self, response: &crate::protocol::Response) -> Result<(), Error> {
        self.store
            .cache_response(&response.request_id, response)
            .map_err(Error::from)
    }

    pub(crate) fn up(
        &mut self,
        request_id: &str,
        source: &str,
        manifest: &Manifest,
    ) -> Result<StackStatus, Error> {
        self.reap_all()?;
        if let Some(existing) = self.store.existing_stack(&manifest.name)? {
            if existing.source != source || existing.service_name != manifest.service.name {
                return Err(Error::Unsupported(
                    "updating an existing stack definition is deferred until update recovery is qualified"
                        .into(),
                ));
            }
            if existing.service_state == "running" && self.children.contains_key(&manifest.name) {
                self.store.record_noop_up(request_id, &manifest.name)?;
                return self.status(&manifest.name)?.ok_or_else(|| {
                    Error::Store(crate::store::Error::NotFound(manifest.name.clone()))
                });
            }
            if !matches!(existing.service_state.as_str(), "stopped" | "failed") {
                return Err(Error::Unsupported(format!(
                    "stack {:?} is {:?}; only a proven stopped rootfs can be reused",
                    manifest.name, existing.service_state
                )));
            }
            self.store.begin_reuse(request_id, &manifest.name)?;
            let (stdout, stderr) = match open_logs(&existing.stdout_log, &existing.stderr_log) {
                Ok(logs) => logs,
                Err(error) => {
                    self.store.mark_failed(
                        request_id,
                        &manifest.name,
                        "log_reopen",
                        &error.to_string(),
                        false,
                    )?;
                    return Err(error);
                }
            };
            return self.start_child(request_id, manifest, &existing.alias, stdout, stderr);
        }
        let alias = generate_alias(
            self.store.installation_id(),
            &manifest.name,
            &manifest.service.name,
        )?;
        let (stdout_path, stderr_path) =
            self.paths.log_paths(&manifest.name, &manifest.service.name);

        self.store.begin_prepare(
            request_id,
            source,
            manifest,
            &alias,
            &stdout_path,
            &stderr_path,
        )?;
        let (stdout, stderr) =
            match prepare_logs(&self.paths, &manifest.name, &stdout_path, &stderr_path) {
                Ok(logs) => logs,
                Err(error) => {
                    self.store.mark_failed(
                        request_id,
                        &manifest.name,
                        "log_prepare",
                        &error.to_string(),
                        false,
                    )?;
                    return Err(error);
                }
            };
        self.store
            .set_operation_phase(request_id, "logs_prepared")?;
        self.store
            .set_operation_phase(request_id, "install_invoked")?;
        if let Err(error) = self.engine.install(&alias, &manifest.service.image) {
            let message = error.to_string();
            self.store
                .mark_failed(request_id, &manifest.name, "engine_install", &message, true)?;
            return Err(Error::Engine(error));
        }
        self.store.mark_installed(request_id, &manifest.name)?;

        self.start_child(request_id, manifest, &alias, stdout, stderr)
    }

    fn start_child(
        &mut self,
        request_id: &str,
        manifest: &Manifest,
        alias: &str,
        stdout: fs::File,
        stderr: fs::File,
    ) -> Result<StackStatus, Error> {
        let mut child =
            match self
                .engine
                .run(alias, manifest.service.command.as_deref(), stdout, stderr)
            {
                Ok(child) => child,
                Err(error) => {
                    let message = error.to_string();
                    self.store.mark_failed(
                        request_id,
                        &manifest.name,
                        "engine_run",
                        &message,
                        false,
                    )?;
                    return Err(Error::Engine(error));
                }
            };
        let pid = child.id();
        let starttime = match engine::process_starttime(pid) {
            Ok(starttime) => starttime,
            Err(error) => {
                self.children.insert(
                    manifest.name.clone(),
                    ManagedChild {
                        child,
                        starttime: 0,
                        boot_id: String::new(),
                        session_id: None,
                    },
                );
                self.store.mark_failed(
                    request_id,
                    &manifest.name,
                    "identity",
                    &error.to_string(),
                    true,
                )?;
                return Err(Error::Io(error));
            }
        };
        let boot_id = engine::boot_id()?;
        self.store
            .mark_starting(request_id, &manifest.name, pid, starttime, &boot_id)?;

        let deadline = Instant::now() + SESSION_TIMEOUT;
        let session_id = loop {
            if let Some(status) = child.try_wait()? {
                let code = status.code();
                self.store.mark_failed(
                    request_id,
                    &manifest.name,
                    "start_exit",
                    &format!("workload exited during startup with code {code:?}"),
                    false,
                )?;
                return Err(Error::ChildExited(code));
            }
            if self.engine.sessions()?.contains(&pid) {
                break pid;
            }
            if Instant::now() >= deadline {
                self.children.insert(
                    manifest.name.clone(),
                    ManagedChild {
                        child,
                        starttime,
                        boot_id,
                        session_id: None,
                    },
                );
                self.store.mark_failed(
                    request_id,
                    &manifest.name,
                    "session_unknown",
                    "engine session registry did not publish the owned child",
                    true,
                )?;
                return Err(Error::SessionUnknown);
            }
            thread::sleep(Duration::from_millis(50));
        };

        self.store
            .mark_running(request_id, &manifest.name, session_id)?;
        self.children.insert(
            manifest.name.clone(),
            ManagedChild {
                child,
                starttime,
                boot_id,
                session_id: Some(session_id),
            },
        );
        self.status(&manifest.name)?
            .ok_or_else(|| Error::Store(crate::store::Error::NotFound(manifest.name.clone())))
    }

    pub(crate) fn status(&mut self, stack: &str) -> Result<Option<StackStatus>, Error> {
        self.reap(stack)?;
        self.store.stack_status(stack).map_err(Error::from)
    }

    pub(crate) fn down(&mut self, request_id: &str, stack: &str) -> Result<StackStatus, Error> {
        self.reap(stack)?;
        let before = self
            .store
            .stack_status(stack)?
            .ok_or_else(|| Error::Store(crate::store::Error::NotFound(stack.to_owned())))?;
        self.store.begin_down(request_id, stack)?;

        let Some(managed) = self.children.get_mut(stack) else {
            if matches!(before.service_state.as_str(), "stopped" | "failed") {
                self.store.mark_stopped(
                    request_id,
                    stack,
                    before.last_exit_code.map(|v| v as i32),
                )?;
                return self
                    .store
                    .stack_status(stack)?
                    .ok_or_else(|| Error::Store(crate::store::Error::NotFound(stack.to_owned())));
            }
            let message = "the daemon no longer owns the workload child handle";
            self.store
                .mark_failed(request_id, stack, "identity_lost", message, true)?;
            return Err(Error::IdentityChanged);
        };
        let identity = self.store.service_identity(stack)?;
        let Some(session) = managed.session_id else {
            self.store.mark_failed(
                request_id,
                stack,
                "session_unknown",
                "the owned child has no qualified engine session",
                true,
            )?;
            return Err(Error::SessionUnknown);
        };
        let current_boot = match engine::boot_id() {
            Ok(value) => value,
            Err(error) => {
                self.store.mark_failed(
                    request_id,
                    stack,
                    "identity_observation",
                    &error.to_string(),
                    true,
                )?;
                return Err(Error::Io(error));
            }
        };
        let current_starttime = match engine::process_starttime(managed.child.id()) {
            Ok(value) => value,
            Err(error) => {
                self.store.mark_failed(
                    request_id,
                    stack,
                    "identity_observation",
                    &error.to_string(),
                    true,
                )?;
                return Err(Error::Io(error));
            }
        };
        let sessions = match self.engine.sessions() {
            Ok(value) => value,
            Err(error) => {
                self.store.mark_failed(
                    request_id,
                    stack,
                    "session_observation",
                    &error.to_string(),
                    true,
                )?;
                return Err(Error::Engine(error));
            }
        };
        if identity.alias != before.alias
            || identity.session_id != Some(session)
            || identity.child_pid != Some(managed.child.id())
            || identity.child_starttime != i64::try_from(managed.starttime).ok()
            || identity.boot_id.as_deref() != Some(managed.boot_id.as_str())
            || current_boot != managed.boot_id
            || current_starttime != managed.starttime
            || !sessions.contains(&session)
        {
            let message = "owned child, persisted identity, and engine session do not agree";
            self.store
                .mark_failed(request_id, stack, "identity_changed", message, true)?;
            return Err(Error::IdentityChanged);
        }

        self.store.set_operation_phase(request_id, "stopping")?;
        if let Err(error) = self.engine.kill(session) {
            self.store
                .mark_failed(request_id, stack, "engine_kill", &error.to_string(), true)?;
            return Err(Error::Engine(error));
        }
        let status = match wait_for_exit(&mut managed.child, STOP_TIMEOUT) {
            Ok(status) => status,
            Err(error) => {
                self.store.mark_failed(
                    request_id,
                    stack,
                    "stop_timeout",
                    &error.to_string(),
                    true,
                )?;
                return Err(error);
            }
        };
        self.store.mark_stopped(request_id, stack, status.code())?;
        self.children.remove(stack);
        self.store
            .stack_status(stack)?
            .ok_or_else(|| Error::Store(crate::store::Error::NotFound(stack.to_owned())))
    }

    pub(crate) fn shutdown(&mut self) {
        let stacks: Vec<String> = self.children.keys().cloned().collect();
        for (index, stack) in stacks.into_iter().enumerate() {
            let request_id = format!("shutdown-{}-{index}", std::process::id());
            if let Err(error) = self.down(&request_id, &stack) {
                eprintln!(
                    "termux-stacks daemon: could not stop stack {stack:?} during shutdown: {error}"
                );
            }
        }
    }

    fn reap_all(&mut self) -> Result<(), Error> {
        let stacks: Vec<String> = self.children.keys().cloned().collect();
        for stack in stacks {
            self.reap(&stack)?;
        }
        Ok(())
    }

    fn reap(&mut self, stack: &str) -> Result<(), Error> {
        let status = match self.children.get_mut(stack) {
            Some(managed) => managed.child.try_wait()?,
            None => None,
        };
        if let Some(status) = status {
            self.store.record_exit(stack, status.code())?;
            self.children.remove(stack);
        }
        Ok(())
    }
}

fn create_log(path: &Path) -> Result<fs::File, Error> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(Error::UnsafeLog(PathBufDisplay(path.to_path_buf())));
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::UnsafeLog(PathBufDisplay(path.to_path_buf())));
    }
    Ok(file)
}

fn open_log_append(path: &Path) -> Result<fs::File, Error> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.permissions().mode() & 0o077 != 0
    {
        return Err(Error::UnsafeLog(PathBufDisplay(path.to_path_buf())));
    }
    let file = OpenOptions::new().append(true).open(path)?;
    let after = file.metadata()?;
    if !after.is_file() || before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(Error::UnsafeLog(PathBufDisplay(path.to_path_buf())));
    }
    Ok(file)
}

fn open_logs(stdout_path: &Path, stderr_path: &Path) -> Result<(fs::File, fs::File), Error> {
    Ok((open_log_append(stdout_path)?, open_log_append(stderr_path)?))
}

fn prepare_logs(
    paths: &RuntimePaths,
    stack: &str,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<(fs::File, fs::File), Error> {
    paths.prepare_stack_log_directory(stack)?;
    let stdout = create_log(stdout_path)?;
    let stderr = create_log(stderr_path)?;
    Ok((stdout, stderr))
}

fn generate_alias(installation_id: &str, stack: &str, service: &str) -> Result<String, Error> {
    let mut random = [0_u8; 6];
    fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "txs-{}-{}-{}-{suffix}",
        &installation_id[..6],
        short_name(stack),
        short_name(service)
    ))
}

fn short_name(name: &str) -> &str {
    &name[..name.len().min(8)]
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(Error::StopTimeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
}
