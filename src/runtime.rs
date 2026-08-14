use crate::engine::{self, Engine, RunOptions};
use crate::manifest::{Manifest, RestartPolicy, Service};
use crate::paths::RuntimePaths;
use crate::store::{ScheduledRestart, ServicePlan, StackStatus, Store};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SESSION_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const STABLE_WINDOW: Duration = Duration::from_secs(60);
const MAX_RESTART_ATTEMPTS: u32 = 5;
const RESTART_DELAYS: [u64; 5] = [1, 2, 4, 8, 16];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ServiceKey {
    stack: String,
    service: String,
}

impl ServiceKey {
    fn new(stack: impl Into<String>, service: impl Into<String>) -> Self {
        Self {
            stack: stack.into(),
            service: service.into(),
        }
    }
}

pub(crate) struct Runtime {
    paths: RuntimePaths,
    store: Store,
    engine: Engine,
    children: BTreeMap<ServiceKey, ManagedChild>,
    uncertain_children: BTreeMap<ServiceKey, Child>,
    restart_throttle: BTreeMap<ServiceKey, Instant>,
}

struct ManagedChild {
    child: Child,
    alias: String,
    generation: i64,
    starttime: u64,
    boot_id: String,
    session_id: Option<u32>,
    restart: RestartPolicy,
    started_at: Instant,
}

#[derive(Debug, Serialize)]
pub(crate) struct LogsResult {
    stack: String,
    service: String,
    tail: u16,
    stdout: Vec<String>,
    stderr: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum Error {
    Store(crate::store::Error),
    Engine(engine::Error),
    Manifest(crate::manifest::Error),
    Resource(crate::resources::Error),
    Logs(crate::logs::Error),
    Io(io::Error),
    UnsafeLog(PathBuf),
    ChildExited(Option<i32>, Option<i32>),
    StartUncertain(String),
    SessionUnknown,
    IdentityChanged,
    StopTimeout,
    ProtocolState(String),
    RestartPending(String),
    Conflict(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "{error}"),
            Self::Engine(error) => write!(formatter, "{error}"),
            Self::Manifest(error) => write!(formatter, "persisted manifest is invalid: {error}"),
            Self::Resource(error) => write!(formatter, "{error}"),
            Self::Logs(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "runtime I/O failed: {error}"),
            Self::UnsafeLog(path) => write!(formatter, "unsafe log path {}", path.display()),
            Self::ChildExited(code, signal) => write!(
                formatter,
                "workload exited during startup with code {code:?} and signal {signal:?}"
            ),
            Self::StartUncertain(message) => write!(formatter, "{message}"),
            Self::SessionUnknown => formatter.write_str(
                "the engine did not publish a qualified session; the workload state is unknown",
            ),
            Self::IdentityChanged => formatter
                .write_str("persisted identity, owned child, and engine session no longer agree"),
            Self::StopTimeout => {
                formatter.write_str("owned workload did not exit after engine kill")
            }
            Self::ProtocolState(message)
            | Self::RestartPending(message)
            | Self::Conflict(message) => formatter.write_str(message),
        }
    }
}

impl Error {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Store(crate::store::Error::Conflict(_)) | Self::Conflict(_) => "conflict",
            Self::Store(crate::store::Error::NotFound(_)) => "not_found",
            Self::Store(_) | Self::ProtocolState(_) | Self::RestartPending(_) => "state_store",
            Self::Engine(_) => "engine",
            Self::Manifest(_) => "invalid_manifest",
            Self::Resource(crate::resources::Error::PortUnavailable(_, _)) => "port_unavailable",
            Self::Resource(_) => "resource",
            Self::Logs(_) | Self::Io(_) | Self::UnsafeLog(_) => "io",
            Self::ChildExited(_, _) => "start_failed",
            Self::StartUncertain(_)
            | Self::SessionUnknown
            | Self::IdentityChanged
            | Self::StopTimeout => "unknown",
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

impl From<crate::manifest::Error> for Error {
    fn from(error: crate::manifest::Error) -> Self {
        Self::Manifest(error)
    }
}

impl From<crate::resources::Error> for Error {
    fn from(error: crate::resources::Error) -> Self {
        Self::Resource(error)
    }
}

impl From<crate::logs::Error> for Error {
    fn from(error: crate::logs::Error) -> Self {
        Self::Logs(error)
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
                "termux-stacks daemon: reconciled {reconciled} interrupted service state(s) after cold start"
            );
        }
        let mut runtime = Self {
            paths,
            store,
            engine,
            children: BTreeMap::new(),
            uncertain_children: BTreeMap::new(),
            restart_throttle: BTreeMap::new(),
        };
        runtime.resume_gracefully_stopped()?;
        Ok(runtime)
    }

    pub(crate) fn installation_id(&self) -> &str {
        self.store.installation_id()
    }

    pub(crate) fn replay_response(
        &self,
        request_id: &str,
        operation: &str,
        stack: &str,
        candidate_manifest: Option<&str>,
        manifest_base: Option<&str>,
        target_service: Option<&str>,
    ) -> Result<Option<crate::protocol::Response>, Error> {
        let Some(replay) = self.store.operation_replay(request_id)? else {
            return Ok(None);
        };
        if !replay_matches(
            &replay,
            operation,
            stack,
            candidate_manifest,
            manifest_base,
            target_service,
        ) {
            return Ok(Some(crate::protocol::Response::failure(
                request_id,
                "request_id_conflict",
                "request_id was already used for a different mutation payload",
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

    pub(crate) fn cache_response_for_request(
        &self,
        request: &crate::protocol::Request,
        response: &crate::protocol::Response,
    ) -> Result<(), Error> {
        if request.validate_envelope().is_err() {
            return Ok(());
        }
        let expected = match request {
            crate::protocol::Request::Up {
                request_id,
                manifest,
                manifest_base,
                ..
            } => {
                let Ok(parsed) = crate::manifest::parse(manifest) else {
                    return Ok(());
                };
                (
                    request_id.as_str(),
                    "up",
                    parsed.name,
                    Some(manifest.as_str()),
                    Some(manifest_base.as_str()),
                    None,
                )
            }
            crate::protocol::Request::Down {
                request_id, stack, ..
            } => (request_id.as_str(), "down", stack.clone(), None, None, None),
            crate::protocol::Request::Restart {
                request_id,
                stack,
                service,
                ..
            } => (
                request_id.as_str(),
                "restart",
                stack.clone(),
                None,
                None,
                Some(service.as_str()),
            ),
            crate::protocol::Request::Status { .. } | crate::protocol::Request::Logs { .. } => {
                return Ok(());
            }
        };
        let Some(replay) = self.store.operation_replay(expected.0)? else {
            return Ok(());
        };
        if replay_matches(
            &replay,
            expected.1,
            &expected.2,
            expected.3,
            expected.4,
            expected.5,
        ) {
            self.store.cache_response(&response.request_id, response)?;
        }
        Ok(())
    }

    pub(crate) fn up(
        &mut self,
        request_id: &str,
        source: &str,
        manifest_base: &str,
        manifest: &Manifest,
    ) -> Result<StackStatus, Error> {
        self.tick()?;
        self.preflight_manifest(manifest_base, manifest)?;

        if let Some(existing) = self.store.existing_stack(&manifest.name)? {
            if existing.observed_state == "unknown"
                || existing.services.iter().any(|service| {
                    service.observed_state == "unknown" || service.rootfs_state == "unknown"
                })
            {
                return Err(Error::Conflict(format!(
                    "stack {:?} has ambiguous state and requires manual intervention",
                    manifest.name
                )));
            }

            let same_definition = existing.source == source
                && existing.manifest_base == Path::new(manifest_base)
                && existing.revision > 0;
            if same_definition {
                if self
                    .uncertain_children
                    .keys()
                    .any(|key| key.stack == manifest.name)
                {
                    return Err(Error::Conflict(format!(
                        "stack {:?} has an unqualified child and cannot be converged safely",
                        manifest.name
                    )));
                }
                let by_name = existing
                    .services
                    .iter()
                    .map(|service| (service.name.as_str(), service))
                    .collect::<BTreeMap<_, _>>();
                let mut missing = Vec::new();
                for service_name in manifest.start_order() {
                    let key = ServiceKey::new(&manifest.name, &service_name);
                    let persisted = by_name
                        .get(service_name.as_str())
                        .expect("manifest and store service sets agree");
                    if self.children.contains_key(&key) {
                        if persisted.desired_state != "running"
                            || persisted.observed_state != "running"
                        {
                            return Err(Error::Conflict(format!(
                                "owned service {:?}/{service_name:?} disagrees with persisted state",
                                manifest.name
                            )));
                        }
                        self.validate_owned_service_running(&key)?;
                    } else if matches!(persisted.observed_state.as_str(), "stopped" | "failed")
                        && persisted.rootfs_state == "installed"
                        && persisted.alias.is_some()
                        && persisted.next_restart_at.is_none()
                    {
                        missing.push(service_name);
                    } else {
                        return Err(Error::Conflict(format!(
                            "service {:?}/{service_name:?} is neither owned-running nor proven startable",
                            manifest.name
                        )));
                    }
                }
                if missing.is_empty() {
                    self.store.record_noop_up(
                        request_id,
                        &manifest.name,
                        source,
                        Path::new(manifest_base),
                    )?;
                    return self.required_status(&manifest.name);
                }
                return self.start_current_services(
                    request_id,
                    "up",
                    manifest,
                    manifest_base,
                    &missing,
                    Some(source),
                );
            }

            if existing.revision > 0 && existing.desired_state != "stopped" {
                return Err(Error::Conflict(format!(
                    "stack {:?} must be stopped explicitly before changing its manifest",
                    manifest.name
                )));
            }
        }

        self.prepare_candidate(request_id, source, manifest_base, manifest)
    }

    fn prepare_candidate(
        &mut self,
        request_id: &str,
        source: &str,
        manifest_base: &str,
        manifest: &Manifest,
    ) -> Result<StackStatus, Error> {
        fault_checkpoint("before_intent")?;
        let mut fresh_plans = BTreeMap::new();
        for service in manifest.services.values() {
            let alias =
                generate_alias(self.store.installation_id(), &manifest.name, &service.name)?;
            let (stdout_log, stderr_log) = self.paths.log_paths(&manifest.name, &service.name);
            fresh_plans.insert(
                service.name.clone(),
                ServicePlan {
                    alias,
                    stdout_log,
                    stderr_log,
                },
            );
        }
        let recovery = match self.store.begin_candidate_recovery(
            request_id,
            source,
            Path::new(manifest_base),
            manifest,
            &fresh_plans,
        ) {
            Ok(recovery) => Some(recovery),
            Err(crate::store::Error::NotFound(_)) => None,
            Err(error) => return Err(Error::Store(error)),
        };
        let plans = if let Some(recovery) = recovery {
            debug_assert!(recovery.revision > 0);
            recovery
                .services
                .into_iter()
                .map(|(name, recovered)| (name, (recovered.plan, recovered.reuse_installed)))
                .collect::<BTreeMap<_, _>>()
        } else {
            self.store.begin_up(
                request_id,
                source,
                Path::new(manifest_base),
                manifest,
                &fresh_plans,
            )?;
            fresh_plans
                .into_iter()
                .map(|(name, plan)| (name, (plan, false)))
                .collect()
        };
        let order = manifest.start_order();
        let mut started = Vec::new();
        if let Err(error) = fault_checkpoint("after_intent") {
            let failed = order.first().expect("manifest has at least one service");
            self.abort_start_operation(request_id, manifest, &order, failed, &started, &error)?;
            return Err(error);
        }
        for (index, service_name) in order.iter().enumerate() {
            let service = &manifest.services[service_name];
            let (plan, reuse_installed) = &plans[service_name];
            let result = if *reuse_installed {
                self.start_installed_candidate(request_id, manifest_base, manifest, service, plan)
            } else {
                self.install_and_start_candidate(request_id, manifest_base, manifest, service, plan)
            };
            if let Err(error) = result {
                self.abort_start_operation(
                    request_id,
                    manifest,
                    &order,
                    service_name,
                    &started,
                    &error,
                )?;
                return Err(error);
            }
            started.push(ServiceKey::new(&manifest.name, service_name));
            if index + 1 < order.len()
                && let Err(error) = fault_checkpoint("between_service_starts")
            {
                self.abort_start_operation(
                    request_id,
                    manifest,
                    &order,
                    service_name,
                    &started,
                    &error,
                )?;
                return Err(error);
            }
        }
        if let Err(error) = fault_checkpoint("before_commit") {
            let failed = order.last().expect("manifest has at least one service");
            self.abort_start_operation(request_id, manifest, &order, failed, &started, &error)?;
            return Err(error);
        }
        if let Err(error) = self.store.commit_up(request_id, &manifest.name) {
            let error = Error::Store(error);
            let failed = order.last().expect("manifest has at least one service");
            self.abort_start_operation(request_id, manifest, &order, failed, &started, &error)?;
            return Err(error);
        }
        self.required_status(&manifest.name)
    }

    fn start_installed_candidate(
        &mut self,
        request_id: &str,
        manifest_base: &str,
        manifest: &Manifest,
        service: &Service,
        plan: &ServicePlan,
    ) -> Result<(), Error> {
        let (stdout, stderr) = open_logs(&plan.stdout_log, &plan.stderr_log)?;
        if let Err(error) = self.ensure_dependencies_running(manifest, service) {
            self.record_operation_failure(request_id, &manifest.name, &service.name, &error);
            return Err(error);
        }
        self.start_operation_child(
            request_id,
            manifest_base,
            manifest,
            service,
            &plan.alias,
            stdout,
            stderr,
        )
    }

    fn install_and_start_candidate(
        &mut self,
        request_id: &str,
        manifest_base: &str,
        manifest: &Manifest,
        service: &Service,
        plan: &ServicePlan,
    ) -> Result<(), Error> {
        let (stdout, stderr) = prepare_existing_logs(
            &self.paths,
            &manifest.name,
            &plan.stdout_log,
            &plan.stderr_log,
        )?;
        self.store
            .mark_logs_prepared(request_id, &manifest.name, &service.name)?;
        self.store
            .mark_install_invoked(request_id, &manifest.name, &service.name)?;
        if let Err(error) = self.engine.install(&plan.alias, &service.image) {
            self.store
                .mark_rootfs_unknown(request_id, &manifest.name, &service.name)?;
            self.store.mark_service_failed(
                request_id,
                &manifest.name,
                &service.name,
                "engine_install",
                &error.to_string(),
                true,
            )?;
            return Err(Error::Engine(error));
        }
        self.store
            .mark_installed(request_id, &manifest.name, &service.name)?;
        if manifest.start_order().first() == Some(&service.name) {
            fault_checkpoint("after_install")?;
        }
        if let Err(error) = self.ensure_dependencies_running(manifest, service) {
            self.record_operation_failure(request_id, &manifest.name, &service.name, &error);
            return Err(error);
        }
        self.start_operation_child(
            request_id,
            manifest_base,
            manifest,
            service,
            &plan.alias,
            stdout,
            stderr,
        )
    }

    fn start_current_stack(
        &mut self,
        request_id: &str,
        operation: &str,
        manifest: &Manifest,
        manifest_base: &str,
    ) -> Result<StackStatus, Error> {
        let order = manifest.start_order();
        self.start_current_services(request_id, operation, manifest, manifest_base, &order, None)
    }

    fn start_current_services(
        &mut self,
        request_id: &str,
        operation: &str,
        manifest: &Manifest,
        manifest_base: &str,
        order: &[String],
        candidate_manifest: Option<&str>,
    ) -> Result<StackStatus, Error> {
        self.store.begin_start_current(
            request_id,
            &manifest.name,
            operation,
            order,
            None,
            candidate_manifest,
            candidate_manifest.map(|_| Path::new(manifest_base)),
        )?;
        let existing = match self
            .store
            .existing_stack(&manifest.name)
            .map_err(Error::from)
            .and_then(|existing| {
                existing.ok_or_else(|| {
                    Error::Store(crate::store::Error::NotFound(format!(
                        "stack {:?}",
                        manifest.name
                    )))
                })
            }) {
            Ok(existing) => existing,
            Err(error) => {
                let failed = order.first().expect("start order has at least one service");
                self.abort_start_operation(request_id, manifest, order, failed, &[], &error)?;
                return Err(error);
            }
        };
        let by_name = existing
            .services
            .iter()
            .map(|service| (service.name.as_str(), service))
            .collect::<BTreeMap<_, _>>();
        let mut started = Vec::new();
        for service_name in order {
            let service = &manifest.services[service_name];
            if let Err(error) = self.ensure_dependencies_running(manifest, service) {
                self.abort_start_operation(
                    request_id,
                    manifest,
                    order,
                    service_name,
                    &started,
                    &error,
                )?;
                return Err(error);
            }
            let prepared = by_name
                .get(service_name.as_str())
                .ok_or_else(|| {
                    Error::Conflict(format!(
                        "service {:?}/{service_name:?} is missing from persisted state",
                        manifest.name
                    ))
                })
                .and_then(|persisted| {
                    let alias = persisted.alias.clone().ok_or_else(|| {
                        Error::Conflict(format!(
                            "service {:?}/{service_name:?} has no committed rootfs alias",
                            manifest.name
                        ))
                    })?;
                    let (stdout, stderr) = open_logs(&persisted.stdout_log, &persisted.stderr_log)?;
                    Ok((alias, stdout, stderr))
                });
            let (alias, stdout, stderr) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.abort_start_operation(
                        request_id,
                        manifest,
                        order,
                        service_name,
                        &started,
                        &error,
                    )?;
                    return Err(error);
                }
            };
            if let Err(error) = self.start_operation_child(
                request_id,
                manifest_base,
                manifest,
                service,
                &alias,
                stdout,
                stderr,
            ) {
                self.abort_start_operation(
                    request_id,
                    manifest,
                    order,
                    service_name,
                    &started,
                    &error,
                )?;
                return Err(error);
            }
            started.push(ServiceKey::new(&manifest.name, service_name));
        }
        if let Err(error) = self.store.finish_start_current(request_id, &manifest.name) {
            let error = Error::Store(error);
            let failed = order.last().expect("start order has at least one service");
            self.abort_start_operation(request_id, manifest, order, failed, &started, &error)?;
            return Err(error);
        }
        self.required_status(&manifest.name)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_operation_child(
        &mut self,
        request_id: &str,
        manifest_base: &str,
        manifest: &Manifest,
        service: &Service,
        alias: &str,
        stdout: fs::File,
        stderr: fs::File,
    ) -> Result<(), Error> {
        self.store
            .mark_start_invoked(request_id, &manifest.name, &service.name)?;
        let result = self.spawn_and_qualify(
            manifest_base,
            manifest,
            service,
            alias,
            stdout,
            stderr,
            StartJournal::Operation(request_id),
        );
        if let Err(error) = &result {
            let unknown = matches!(
                error,
                Error::Store(_)
                    | Error::StartUncertain(_)
                    | Error::SessionUnknown
                    | Error::IdentityChanged
            );
            let _ = self.store.mark_service_failed(
                request_id,
                &manifest.name,
                &service.name,
                error.code(),
                &error.to_string(),
                unknown,
            );
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_and_qualify(
        &mut self,
        manifest_base: &str,
        manifest: &Manifest,
        service: &Service,
        alias: &str,
        stdout: fs::File,
        stderr: fs::File,
        journal: StartJournal<'_>,
    ) -> Result<(), Error> {
        crate::resources::preflight_ports(service)?;
        let binds = crate::resources::resolve_binds(&self.paths, manifest_base, manifest, service)?;
        let key = ServiceKey::new(&manifest.name, &service.name);
        if self.children.contains_key(&key) || self.uncertain_children.contains_key(&key) {
            return Err(Error::IdentityChanged);
        }
        let child = self.engine.run(
            alias,
            RunOptions {
                command: service.command.as_deref(),
                environment: &service.environment,
                binds: &binds,
            },
            stdout,
            stderr,
        )?;
        let pid = child.id();
        let starttime = match engine::process_starttime(pid) {
            Ok(value) => value,
            Err(error) => {
                let drained = self.contain_unqualified_child(key, child, pid);
                return Err(Error::StartUncertain(format!(
                    "could not qualify spawned process identity: {error}; engine session drained: {drained}"
                )));
            }
        };
        let boot_id = match engine::boot_id() {
            Ok(value) => value,
            Err(error) => {
                let drained = self.contain_unqualified_child(key, child, pid);
                return Err(Error::StartUncertain(format!(
                    "could not read the boot identity after spawning: {error}; engine session drained: {drained}"
                )));
            }
        };
        let journal_result = match journal {
            StartJournal::Operation(request_id) => self.store.mark_starting(
                request_id,
                &manifest.name,
                &service.name,
                pid,
                starttime,
                &boot_id,
            ),
            StartJournal::Restart => self.store.mark_restart_starting(
                &manifest.name,
                &service.name,
                pid,
                starttime,
                &boot_id,
            ),
        };
        if let Err(error) = journal_result {
            let drained = self.contain_unqualified_child(key, child, pid);
            return Err(Error::StartUncertain(format!(
                "could not persist the spawned process identity: {error}; engine session drained: {drained}"
            )));
        }
        let persisted_identity = match self.store.service_identity(&manifest.name, &service.name) {
            Ok(identity) => identity,
            Err(error) => {
                let drained = self.contain_unqualified_child(key, child, pid);
                return Err(Error::StartUncertain(format!(
                    "could not reread the persisted process identity: {error}; engine session drained: {drained}"
                )));
            }
        };
        if persisted_identity.alias != alias {
            let _ = self.contain_unqualified_child(key, child, pid);
            return Err(Error::IdentityChanged);
        }
        self.children.insert(
            key.clone(),
            ManagedChild {
                child,
                alias: alias.to_owned(),
                generation: persisted_identity.generation,
                starttime,
                boot_id,
                session_id: None,
                restart: service.restart,
                started_at: Instant::now(),
            },
        );
        let qualification = (|| -> Result<(), Error> {
            if matches!(journal, StartJournal::Operation(_))
                && manifest.start_order().first() == Some(&service.name)
            {
                fault_checkpoint("after_start").map_err(|error| {
                    Error::StartUncertain(format!(
                        "fault checkpoint failed after spawning {:?}/{:?}: {error}",
                        manifest.name, service.name
                    ))
                })?;
            }

            let deadline = Instant::now() + SESSION_TIMEOUT;
            let session_id = loop {
                let status = self
                    .children
                    .get_mut(&key)
                    .expect("owned child inserted")
                    .child
                    .try_wait()
                    .map_err(|error| {
                        Error::StartUncertain(format!(
                            "could not observe spawned child {:?}/{:?}: {error}",
                            manifest.name, service.name
                        ))
                    })?;
                if let Some(status) = status {
                    self.children.remove(&key);
                    let (code, signal) = exit_parts(&status);
                    let registry_ambiguous = self
                        .engine
                        .sessions()
                        .map_or(true, |sessions| sessions.contains(&pid));
                    if signal.is_some() || registry_ambiguous {
                        let _ = self
                            .store
                            .mark_runtime_unknown(&manifest.name, &service.name);
                        return Err(Error::StartUncertain(format!(
                            "spawned child {:?}/{:?} exited without proven guest-tree absence",
                            manifest.name, service.name
                        )));
                    }
                    self.store
                        .record_exit(&manifest.name, &service.name, code, signal)?;
                    return Err(Error::ChildExited(code, signal));
                }
                let sessions = self.engine.sessions().map_err(|error| {
                    Error::StartUncertain(format!(
                        "could not observe the engine session for {:?}/{:?}: {error}",
                        manifest.name, service.name
                    ))
                })?;
                if sessions.contains(&pid) {
                    break pid;
                }
                if Instant::now() >= deadline {
                    return Err(Error::SessionUnknown);
                }
                thread::sleep(Duration::from_millis(50));
            };
            self.children
                .get_mut(&key)
                .expect("owned child inserted")
                .session_id = Some(session_id);
            match journal {
                StartJournal::Operation(request_id) => self.store.mark_running(
                    request_id,
                    &manifest.name,
                    &service.name,
                    session_id,
                )?,
                StartJournal::Restart => {
                    self.store
                        .mark_restart_running(&manifest.name, &service.name, session_id)?
                }
            }
            Ok(())
        })();

        if qualification.is_err()
            && let Some(managed) = self.children.remove(&key)
        {
            let _ = self
                .store
                .mark_runtime_unknown(&manifest.name, &service.name);
            self.uncertain_children.insert(key, managed.child);
        }
        qualification
    }

    fn ensure_dependencies_running(
        &mut self,
        manifest: &Manifest,
        service: &Service,
    ) -> Result<(), Error> {
        for dependency in &service.depends_on {
            let key = ServiceKey::new(&manifest.name, dependency);
            if !self.children.contains_key(&key) {
                return Err(Error::Conflict(format!(
                    "dependency {:?}/{dependency:?} is not owned by this daemon",
                    manifest.name
                )));
            }
            self.validate_owned_service_running(&key).map_err(|error| {
                Error::Conflict(format!(
                    "dependency {:?}/{dependency:?} is not qualified running for service {:?}: {error}",
                    manifest.name, service.name
                ))
            })?;
        }
        Ok(())
    }

    fn validate_owned_service_running(&mut self, key: &ServiceKey) -> Result<(), Error> {
        if self.uncertain_children.contains_key(key) {
            return Err(Error::IdentityChanged);
        }
        let status = match self
            .children
            .get_mut(key)
            .ok_or(Error::IdentityChanged)?
            .child
            .try_wait()
        {
            Ok(status) => status,
            Err(error) => {
                self.quarantine_managed_child(key);
                return Err(Error::StartUncertain(format!(
                    "could not observe owned child {:?}/{:?}: {error}",
                    key.stack, key.service
                )));
            }
        };
        if let Some(status) = status {
            let (code, signal) = exit_parts(&status);
            let session = self
                .children
                .get(key)
                .and_then(|managed| managed.session_id);
            let registry_ambiguous = match session {
                Some(session) => self
                    .engine
                    .sessions()
                    .map_or(true, |sessions| sessions.contains(&session)),
                None => true,
            };
            let managed = self.children.remove(key).expect("observed child exists");
            if signal.is_some() || registry_ambiguous {
                let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                return Err(Error::IdentityChanged);
            }
            self.store
                .record_exit(&key.stack, &key.service, code, signal)?;
            if should_restart(managed.restart, &status) {
                self.schedule_restart_after_exit(key, &managed)?;
            }
            return Err(Error::Conflict(format!(
                "service {:?}/{:?} exited before it could be revalidated",
                key.stack, key.service
            )));
        }

        let (pid, alias, generation, starttime, boot_id, session) = {
            let managed = self.children.get(key).expect("owned child exists");
            (
                managed.child.id(),
                managed.alias.clone(),
                managed.generation,
                managed.starttime,
                managed.boot_id.clone(),
                managed.session_id,
            )
        };
        let validation = (|| -> Result<(), Error> {
            let session = session.ok_or(Error::SessionUnknown)?;
            let identity = self.store.service_identity(&key.stack, &key.service)?;
            let current_boot = engine::boot_id()?;
            let current_starttime = engine::process_starttime(pid)?;
            let sessions = self.engine.sessions().map_err(|error| {
                Error::StartUncertain(format!(
                    "could not revalidate owned child {:?}/{:?}: {error}",
                    key.stack, key.service
                ))
            })?;
            if identity.session_id != Some(session)
                || identity.alias != alias
                || identity.generation != generation
                || identity.child_pid != Some(pid)
                || identity.child_starttime != i64::try_from(starttime).ok()
                || identity.boot_id.as_deref() != Some(boot_id.as_str())
                || current_boot != boot_id
                || current_starttime != starttime
                || !sessions.contains(&session)
            {
                return Err(Error::IdentityChanged);
            }
            Ok(())
        })();
        if validation.is_err() {
            self.quarantine_managed_child(key);
        }
        validation
    }

    fn quarantine_managed_child(&mut self, key: &ServiceKey) {
        if let Some(managed) = self.children.remove(key) {
            let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
            self.uncertain_children.insert(key.clone(), managed.child);
        }
    }

    fn record_operation_failure(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        error: &Error,
    ) {
        let unknown = matches!(
            error,
            Error::Store(_)
                | Error::StartUncertain(_)
                | Error::SessionUnknown
                | Error::IdentityChanged
        );
        let _ = self.store.mark_service_failed(
            request_id,
            stack,
            service,
            error.code(),
            &error.to_string(),
            unknown,
        );
    }

    /// Contains the narrow window after spawn and before full identity
    /// qualification. Only a positive engine-session observation authorizes a
    /// stop. Otherwise the child handle is retained and the service remains
    /// unknown; killing only the tracer could strand guest descendants.
    fn contain_unqualified_child(&mut self, key: ServiceKey, mut child: Child, pid: u32) -> bool {
        let deadline = Instant::now() + SESSION_TIMEOUT;
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if self
                .engine
                .sessions()
                .ok()
                .is_some_and(|sessions| sessions.contains(&pid))
                && self.engine.kill(pid).is_ok()
                && wait_for_exit(&mut child, STOP_TIMEOUT).is_ok()
            {
                return true;
            }
            if Instant::now() >= deadline {
                self.uncertain_children.insert(key, child);
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn abort_start_operation(
        &mut self,
        request_id: &str,
        manifest: &Manifest,
        operation_order: &[String],
        failed_service: &str,
        started: &[ServiceKey],
        error: &Error,
    ) -> Result<(), Error> {
        self.record_operation_failure(request_id, &manifest.name, failed_service, error);
        for service in operation_order {
            if service == failed_service {
                continue;
            }
            let key = ServiceKey::new(&manifest.name, service);
            if started.contains(&key) {
                continue;
            }
            let _ = self.store.mark_service_failed(
                request_id,
                &manifest.name,
                service,
                "dependency_failed",
                &format!("service {failed_service:?} failed before the stack committed: {error}"),
                false,
            );
        }
        self.stop_started_after_failed_up(request_id, started);
        self.store.finalize_operation_failure(
            request_id,
            &manifest.name,
            error.code(),
            &error.to_string(),
        )?;
        Ok(())
    }

    fn stop_started_after_failed_up(&mut self, request_id: &str, started: &[ServiceKey]) -> bool {
        for (reverse_index, key) in started.iter().rev().enumerate() {
            match self.stop_owned_child(key) {
                Ok(status) => {
                    let (code, signal) = exit_parts(&status);
                    if let Err(error) =
                        self.store
                            .record_exit(&key.stack, &key.service, code, signal)
                    {
                        let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                        eprintln!(
                            "termux-stacks daemon: failed to record cleanup of {:?}/{:?}: {error}",
                            key.stack, key.service
                        );
                        self.quarantine_cleanup_survivors(started, reverse_index + 1);
                        return false;
                    }
                }
                Err(error) => {
                    self.quarantine_managed_child(key);
                    self.record_operation_failure(request_id, &key.stack, &key.service, &error);
                    eprintln!(
                        "termux-stacks daemon: failed to stop {:?}/{:?} after partial start: {error}",
                        key.stack, key.service
                    );
                    self.quarantine_cleanup_survivors(started, reverse_index + 1);
                    return false;
                }
            }
        }
        true
    }

    fn quarantine_cleanup_survivors(&mut self, started: &[ServiceKey], already_considered: usize) {
        let survivor_count = started.len().saturating_sub(already_considered);
        for key in started.iter().take(survivor_count) {
            self.quarantine_managed_child(key);
        }
    }

    pub(crate) fn status(&mut self, stack: &str) -> Result<Option<StackStatus>, Error> {
        self.tick()?;
        self.store.stack_status(stack).map_err(Error::from)
    }

    pub(crate) fn down(&mut self, request_id: &str, stack: &str) -> Result<StackStatus, Error> {
        self.tick()?;
        let before = self
            .store
            .stack_status(stack)?
            .ok_or_else(|| crate::store::Error::NotFound(format!("stack {stack:?}")))?;
        let manifest = self.committed_manifest(stack)?;
        let stop_order = manifest
            .as_ref()
            .map(Manifest::stop_order)
            .unwrap_or_else(|| {
                let mut services = before
                    .services
                    .iter()
                    .map(|service| service.name.clone())
                    .collect::<Vec<_>>();
                services.sort();
                services.reverse();
                services
            });

        self.store.begin_down(request_id, stack, &stop_order)?;
        let result = (|| -> Result<(), Error> {
            let after_intent = self.required_status(stack)?;
            for (index, service_name) in stop_order.iter().enumerate() {
                let journaled = after_intent
                    .services
                    .iter()
                    .find(|service| service.name == *service_name)
                    .expect("down order was checked against service set");
                let key = ServiceKey::new(stack, service_name);
                if self.children.contains_key(&key) {
                    self.store
                        .mark_stop_invoked(request_id, stack, service_name)?;
                    if index == 0 {
                        fault_checkpoint("during_down")?;
                    }
                    let status = self.stop_owned_child(&key)?;
                    let (code, signal) = exit_parts(&status);
                    self.store
                        .mark_stopped(request_id, stack, service_name, code, signal)?;
                } else if journaled.observed_state != "stopped"
                    && journaled.observed_state != "unknown"
                {
                    return Err(Error::IdentityChanged);
                }
            }
            self.store.finish_down(request_id, stack)?;
            Ok(())
        })();
        if let Err(error) = result {
            let remaining = self
                .children
                .keys()
                .filter(|key| key.stack == stack)
                .cloned()
                .collect::<Vec<_>>();
            for key in remaining {
                self.quarantine_managed_child(&key);
            }
            self.store
                .finalize_down_failure(request_id, stack, error.code(), &error.to_string())
                .map_err(|finalize_error| {
                    Error::ProtocolState(format!(
                        "down failed: {error}; the failure journal could not be terminalized: {finalize_error}"
                    ))
                })?;
            return Err(error);
        }
        self.required_status(stack)
    }

    pub(crate) fn logs(
        &mut self,
        stack: &str,
        service: &str,
        tail: u16,
    ) -> Result<LogsResult, Error> {
        self.tick()?;
        let status = self
            .store
            .stack_status(stack)?
            .ok_or_else(|| crate::store::Error::NotFound(format!("stack {stack:?}")))?;
        let service_status = status
            .services
            .iter()
            .find(|candidate| candidate.name == service)
            .ok_or_else(|| {
                crate::store::Error::NotFound(format!("service {stack:?}/{service:?}"))
            })?;
        Ok(LogsResult {
            stack: stack.to_owned(),
            service: service.to_owned(),
            tail,
            stdout: crate::logs::tail(Path::new(&service_status.stdout_log), tail)?,
            stderr: crate::logs::tail(Path::new(&service_status.stderr_log), tail)?,
        })
    }

    pub(crate) fn restart(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<StackStatus, Error> {
        self.tick()?;
        let manifest = self
            .committed_manifest(stack)?
            .ok_or_else(|| Error::Conflict(format!("stack {stack:?} has no committed revision")))?;
        let definition = manifest.services.get(service).ok_or_else(|| {
            crate::store::Error::NotFound(format!("service {stack:?}/{service:?}"))
        })?;
        self.ensure_dependencies_running(&manifest, definition)?;
        let key = ServiceKey::new(stack, service);
        if !self.children.contains_key(&key) {
            return Err(Error::IdentityChanged);
        }
        self.store.begin_restart(request_id, stack, service)?;
        if let Err(error) = fault_checkpoint("restart_stop_intent") {
            self.abort_restart_operation(request_id, stack, service, &error, false)?;
            return Err(error);
        }
        if let Err(error) = self
            .store
            .mark_restart_stop_invoked(request_id, stack, service)
        {
            let error = Error::Store(error);
            self.abort_restart_operation(request_id, stack, service, &error, false)?;
            return Err(error);
        }
        let status = match self.stop_owned_child(&key) {
            Ok(status) => status,
            Err(error) => {
                self.abort_restart_operation(request_id, stack, service, &error, false)?;
                return Err(error);
            }
        };
        let (code, signal) = exit_parts(&status);
        if let Err(error) = self
            .store
            .mark_restart_stopped(request_id, stack, service, code, signal)
        {
            let error = Error::Store(error);
            self.abort_restart_operation(request_id, stack, service, &error, false)?;
            return Err(error);
        }
        let restart_result = (|| -> Result<(), Error> {
            fault_checkpoint("restart_stopped")?;
            let existing = self
                .store
                .existing_stack(stack)?
                .ok_or_else(|| crate::store::Error::NotFound(format!("stack {stack:?}")))?;
            let persisted = existing
                .services
                .iter()
                .find(|candidate| candidate.name == service)
                .expect("manifest and store service sets agree");
            let alias = persisted.alias.as_deref().ok_or_else(|| {
                Error::Conflict(format!(
                    "service {stack:?}/{service:?} has no current rootfs"
                ))
            })?;
            let base = existing.manifest_base.to_str().ok_or_else(|| {
                Error::Conflict("persisted manifest base is not UTF-8".to_owned())
            })?;
            self.ensure_dependencies_running(&manifest, definition)?;
            let (stdout, stderr) = open_logs(&persisted.stdout_log, &persisted.stderr_log)?;
            self.start_operation_child(
                request_id, base, &manifest, definition, alias, stdout, stderr,
            )?;
            self.store.finish_restart(request_id, stack, service)?;
            Ok(())
        })();
        if let Err(error) = restart_result {
            self.abort_restart_operation(request_id, stack, service, &error, true)?;
            return Err(error);
        }
        self.required_status(stack)
    }

    fn abort_restart_operation(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        error: &Error,
        cleanup_started_child: bool,
    ) -> Result<(), Error> {
        self.record_operation_failure(request_id, stack, service, error);
        let key = ServiceKey::new(stack, service);
        if self.children.contains_key(&key) {
            if cleanup_started_child {
                self.stop_started_after_failed_up(request_id, &[key]);
            } else {
                self.quarantine_managed_child(&key);
            }
        }
        self.store.finalize_operation_failure(
            request_id,
            stack,
            error.code(),
            &error.to_string(),
        )?;
        Ok(())
    }

    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        let uncertain_keys = self.uncertain_children.keys().cloned().collect::<Vec<_>>();
        for key in uncertain_keys {
            match self
                .uncertain_children
                .get_mut(&key)
                .expect("uncertain child key snapshot")
                .try_wait()
            {
                Ok(Some(status)) => {
                    self.uncertain_children.remove(&key);
                    eprintln!(
                        "termux-stacks daemon: unqualified child {:?}/{:?} exited with {status}; state remains unknown",
                        key.stack, key.service
                    );
                }
                Ok(None) => {}
                Err(error) => eprintln!(
                    "termux-stacks daemon: cannot observe unqualified child {:?}/{:?}: {error}",
                    key.stack, key.service
                ),
            }
        }

        let keys = self.children.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let status = match self
                .children
                .get_mut(&key)
                .expect("child key snapshot")
                .child
                .try_wait()
            {
                Ok(status) => status,
                Err(error) => {
                    let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                    eprintln!(
                        "termux-stacks daemon: cannot observe child {:?}/{:?}; state is unknown: {error}",
                        key.stack, key.service
                    );
                    continue;
                }
            };
            let Some(status) = status else {
                continue;
            };
            let managed = self.children.get(&key).expect("exited child exists");
            let (code, signal) = exit_parts(&status);
            let registry_ambiguous = match managed.session_id {
                Some(session) => match self.engine.sessions() {
                    Ok(sessions) => sessions.contains(&session),
                    Err(_) => true,
                },
                None => true,
            };
            if signal.is_some() || registry_ambiguous {
                match self.store.mark_runtime_unknown(&key.stack, &key.service) {
                    Ok(()) => {
                        self.children.remove(&key);
                        eprintln!(
                            "termux-stacks daemon: child {:?}/{:?} exited without proven guest-tree absence; state is unknown",
                            key.stack, key.service
                        );
                    }
                    Err(error) => eprintln!(
                        "termux-stacks daemon: cannot persist unknown state for {:?}/{:?}: {error}",
                        key.stack, key.service
                    ),
                }
                continue;
            }
            if let Err(error) = self
                .store
                .record_exit(&key.stack, &key.service, code, signal)
            {
                eprintln!(
                    "termux-stacks daemon: cannot persist child exit for {:?}/{:?}: {error}",
                    key.stack, key.service
                );
                continue;
            }
            let managed = self.children.remove(&key).expect("recorded child exists");
            if should_restart(managed.restart, &status)
                && let Err(error) = self.schedule_restart_after_exit(&key, &managed)
            {
                eprintln!(
                    "termux-stacks daemon: cannot schedule restart for {:?}/{:?}: {error}",
                    key.stack, key.service
                );
            }
        }

        let now = unix_time()?;
        let due = self.store.due_restarts(now)?;
        for restart in due {
            debug_assert!(restart.at <= now);
            if restart.attempts <= 0 || restart.attempts > i64::from(MAX_RESTART_ATTEMPTS) {
                let _ = self
                    .store
                    .mark_runtime_unknown(&restart.stack_name, &restart.service_name);
                eprintln!(
                    "termux-stacks daemon: invalid persisted restart attempt for {:?}/{:?}; state is unknown",
                    restart.stack_name, restart.service_name
                );
                continue;
            }
            let key = ServiceKey::new(&restart.stack_name, &restart.service_name);
            if self
                .restart_throttle
                .get(&key)
                .is_some_and(|deadline| *deadline > Instant::now())
            {
                continue;
            }
            self.restart_throttle.remove(&key);
            if self.children.contains_key(&key) || self.uncertain_children.contains_key(&key) {
                let _ = self
                    .store
                    .mark_runtime_unknown(&restart.stack_name, &restart.service_name);
                eprintln!(
                    "termux-stacks daemon: restart deadline collided with an owned child for {:?}/{:?}; state is unknown",
                    restart.stack_name, restart.service_name
                );
                continue;
            }
            if let Err(error) = self.perform_scheduled_restart(&restart) {
                match error {
                    Error::ChildExited(code, signal) => {
                        if let Err(schedule_error) =
                            self.reschedule_after_early_exit(&restart, code, signal)
                        {
                            eprintln!(
                                "termux-stacks daemon: restart candidate {:?}/{:?} exited and could not be rescheduled: {schedule_error}",
                                restart.stack_name, restart.service_name
                            );
                        }
                    }
                    Error::RestartPending(message) => {
                        self.restart_throttle
                            .insert(key, Instant::now() + Duration::from_secs(1));
                        eprintln!(
                            "termux-stacks daemon: restart of {:?}/{:?} remains pending: {message}",
                            restart.stack_name, restart.service_name
                        );
                    }
                    error => {
                        let _ = self
                            .store
                            .mark_runtime_unknown(&restart.stack_name, &restart.service_name);
                        eprintln!(
                            "termux-stacks daemon: restart of {:?}/{:?} became ambiguous: {error}",
                            restart.stack_name, restart.service_name
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn reschedule_after_early_exit(
        &mut self,
        restart: &ScheduledRestart,
        code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), Error> {
        let existing = self
            .store
            .existing_stack(&restart.stack_name)?
            .ok_or_else(|| {
                crate::store::Error::NotFound(format!("stack {:?}", restart.stack_name))
            })?;
        let manifest = crate::manifest::parse(&existing.source)?;
        let definition = manifest
            .services
            .get(&restart.service_name)
            .ok_or_else(|| {
                crate::store::Error::NotFound(format!(
                    "service {:?}/{:?}",
                    restart.stack_name, restart.service_name
                ))
            })?;
        if !should_restart_parts(definition.restart, code, signal) {
            return Ok(());
        }
        let persisted = existing
            .services
            .iter()
            .find(|service| service.name == restart.service_name)
            .ok_or_else(|| {
                crate::store::Error::NotFound(format!(
                    "service {:?}/{:?}",
                    restart.stack_name, restart.service_name
                ))
            })?;
        let now = unix_time()?;
        let active_window = persisted.restart_window_started_at.filter(|started| {
            now.checked_sub(*started)
                .is_some_and(|elapsed| elapsed >= 0 && elapsed < STABLE_WINDOW.as_secs() as i64)
        });
        let (previous, window_started_at) = match active_window {
            Some(started) => (u32::try_from(restart.attempts).unwrap_or(u32::MAX), started),
            None => (0, now),
        };
        if previous >= MAX_RESTART_ATTEMPTS {
            return Ok(());
        }
        let attempts = previous + 1;
        let delay = restart_delay_seconds(previous);
        self.store.schedule_restart(
            &restart.stack_name,
            &restart.service_name,
            attempts,
            window_started_at,
            now + i64::try_from(delay).expect("restart delay fits i64"),
        )?;
        Ok(())
    }

    fn schedule_restart_after_exit(
        &mut self,
        key: &ServiceKey,
        managed: &ManagedChild,
    ) -> Result<(), Error> {
        let existing = self
            .store
            .existing_stack(&key.stack)?
            .ok_or_else(|| crate::store::Error::NotFound(format!("stack {:?}", key.stack)))?;
        if existing.desired_state != "running" {
            return Ok(());
        }
        let persisted = existing
            .services
            .iter()
            .find(|service| service.name == key.service)
            .ok_or_else(|| {
                crate::store::Error::NotFound(format!("service {:?}/{:?}", key.stack, key.service))
            })?;
        let _config: serde_json::Value =
            serde_json::from_str(&persisted.config_json).map_err(|error| {
                Error::ProtocolState(format!("persisted service config is invalid: {error}"))
            })?;
        let now = unix_time()?;
        let existing_window = persisted.restart_window_started_at.filter(|started| {
            now.checked_sub(*started)
                .is_some_and(|elapsed| elapsed >= 0 && elapsed < STABLE_WINDOW.as_secs() as i64)
        });
        let (previous, window_started_at) = if managed.started_at.elapsed() >= STABLE_WINDOW {
            (0, now)
        } else if let Some(started) = existing_window {
            (
                u32::try_from(persisted.restart_attempts).unwrap_or(u32::MAX),
                started,
            )
        } else {
            (0, now)
        };
        if previous >= MAX_RESTART_ATTEMPTS {
            return Ok(());
        }
        let attempts = previous + 1;
        let delay = restart_delay_seconds(previous);
        self.store.schedule_restart(
            &key.stack,
            &key.service,
            attempts,
            window_started_at,
            now + delay as i64,
        )?;
        fault_checkpoint("during_backoff")?;
        Ok(())
    }

    fn perform_scheduled_restart(&mut self, restart: &ScheduledRestart) -> Result<(), Error> {
        let stack = restart.stack_name.as_str();
        let service = restart.service_name.as_str();
        let prepared = (|| -> Result<_, Error> {
            let existing = self
                .store
                .existing_stack(stack)?
                .ok_or_else(|| crate::store::Error::NotFound(format!("stack {stack:?}")))?;
            let manifest = crate::manifest::parse(&existing.source)?;
            let definition = manifest.services.get(service).cloned().ok_or_else(|| {
                crate::store::Error::NotFound(format!("service {stack:?}/{service:?}"))
            })?;
            let persisted = existing
                .services
                .iter()
                .find(|candidate| candidate.name == service)
                .expect("manifest and store service sets agree");
            if persisted.desired_state != "running"
                || persisted.effect_phase != "backoff"
                || persisted.next_restart_at.is_none()
            {
                return Err(Error::IdentityChanged);
            }
            let alias = persisted.alias.clone().ok_or_else(|| {
                Error::Conflict(format!(
                    "service {stack:?}/{service:?} has no current rootfs"
                ))
            })?;
            let base = existing
                .manifest_base
                .to_str()
                .ok_or_else(|| Error::Conflict("persisted manifest base is not UTF-8".to_owned()))?
                .to_owned();
            self.ensure_dependencies_running(&manifest, &definition)?;
            let (stdout, stderr) = open_logs(&persisted.stdout_log, &persisted.stderr_log)?;
            Ok((manifest, definition, alias, base, stdout, stderr))
        })();
        let (manifest, definition, alias, base, stdout, stderr) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let attempt = u32::try_from(restart.attempts).unwrap_or(MAX_RESTART_ATTEMPTS);
                let delay = restart_delay_seconds(attempt.saturating_sub(1));
                self.store.defer_restart(
                    stack,
                    service,
                    unix_time()? + i64::try_from(delay).expect("restart delay fits i64"),
                ).map_err(|store_error| Error::RestartPending(format!(
                    "pre-effect error {error}; the durable retry deadline could not be moved: {store_error}"
                )))?;
                eprintln!(
                    "termux-stacks daemon: proven-absent restart of {stack:?}/{service:?} remains pending after a pre-effect error: {error}"
                );
                return Ok(());
            }
        };
        self.store
            .mark_restart_invoked(stack, service)
            .map_err(|store_error| {
                Error::RestartPending(format!(
                    "the restart intent could not be persisted before spawn: {store_error}"
                ))
            })?;
        let result = self.spawn_and_qualify(
            &base,
            &manifest,
            &definition,
            &alias,
            stdout,
            stderr,
            StartJournal::Restart,
        );
        match result {
            Err(error @ Error::Resource(_))
            | Err(error @ Error::Engine(engine::Error::Spawn { .. }))
            | Err(error @ Error::Engine(engine::Error::InvalidArgument(_))) => {
                let attempt = u32::try_from(restart.attempts).unwrap_or(MAX_RESTART_ATTEMPTS);
                let delay = restart_delay_seconds(attempt.saturating_sub(1));
                if let Err(store_error) = self.store.restore_restart_backoff(
                    stack,
                    service,
                    unix_time()? + i64::try_from(delay).expect("restart delay fits i64"),
                ) {
                    self.store
                        .mark_runtime_unknown(stack, service)
                        .map_err(|unknown_error| {
                            Error::ProtocolState(format!(
                                "proven pre-spawn failure {error}; durable backoff restoration failed: {store_error}; unknown state could not be persisted: {unknown_error}"
                            ))
                        })?;
                    return Err(Error::ProtocolState(format!(
                        "proven pre-spawn failure {error}; durable backoff restoration failed and the service was marked unknown: {store_error}"
                    )));
                }
                eprintln!(
                    "termux-stacks daemon: proven-absent restart of {stack:?}/{service:?} remains pending after a pre-spawn error: {error}"
                );
                Ok(())
            }
            other => other,
        }
    }

    fn preflight_manifest(&self, manifest_base: &str, manifest: &Manifest) -> Result<(), Error> {
        let base = Path::new(manifest_base);
        if !base.is_absolute() {
            return Err(Error::Conflict("manifest_base must be absolute".into()));
        }
        for service in manifest.services.values() {
            for mount in &service.mounts {
                if mount.kind == crate::manifest::MountKind::Bind {
                    let configured = Path::new(&mount.source);
                    let candidate = if configured.is_absolute() {
                        configured.to_path_buf()
                    } else {
                        base.join(configured)
                    };
                    if fs::canonicalize(&candidate).is_err() {
                        return Err(Error::Resource(crate::resources::Error::InvalidBind(
                            candidate,
                        )));
                    }
                }
            }
        }
        let requested = manifest
            .services
            .values()
            .flat_map(|service| service.ports.iter().map(|port| port.port))
            .collect::<BTreeSet<_>>();
        for status in self.store.stack_statuses()? {
            if status.name == manifest.name || status.desired_state != "running" {
                continue;
            }
            let Some(existing) = self.store.existing_stack(&status.name)? else {
                continue;
            };
            if existing.source.is_empty() {
                continue;
            }
            let committed = crate::manifest::parse(&existing.source)?;
            for port in committed
                .services
                .values()
                .flat_map(|service| service.ports.iter().map(|port| port.port))
            {
                if requested.contains(&port) {
                    return Err(Error::Conflict(format!(
                        "loopback port {port} is already declared by running stack {:?}",
                        status.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn committed_manifest(&self, stack: &str) -> Result<Option<Manifest>, Error> {
        let Some(existing) = self.store.existing_stack(stack)? else {
            return Ok(None);
        };
        if existing.revision == 0 || existing.source.is_empty() {
            Ok(None)
        } else {
            crate::manifest::parse(&existing.source)
                .map(Some)
                .map_err(Error::from)
        }
    }

    fn stop_owned_child(&mut self, key: &ServiceKey) -> Result<ExitStatus, Error> {
        let identity = self.store.service_identity(&key.stack, &key.service)?;
        let managed = self.children.get_mut(key).ok_or(Error::IdentityChanged)?;
        let session = managed.session_id.ok_or(Error::SessionUnknown)?;
        let current_boot = engine::boot_id()?;
        let current_starttime = engine::process_starttime(managed.child.id())?;
        let sessions = self.engine.sessions()?;
        if identity.session_id != Some(session)
            || identity.alias != managed.alias
            || identity.generation != managed.generation
            || identity.child_pid != Some(managed.child.id())
            || identity.child_starttime != i64::try_from(managed.starttime).ok()
            || identity.boot_id.as_deref() != Some(managed.boot_id.as_str())
            || current_boot != managed.boot_id
            || current_starttime != managed.starttime
            || !sessions.contains(&session)
        {
            return Err(Error::IdentityChanged);
        }
        self.engine.kill(session)?;
        let status = wait_for_exit(&mut managed.child, STOP_TIMEOUT)?;
        self.children.remove(key);
        Ok(status)
    }

    fn required_status(&self, stack: &str) -> Result<StackStatus, Error> {
        self.store
            .stack_status(stack)?
            .ok_or_else(|| Error::Store(crate::store::Error::NotFound(format!("stack {stack:?}"))))
    }

    fn resume_gracefully_stopped(&mut self) -> Result<(), Error> {
        let statuses = self.store.stack_statuses()?;
        for status in statuses {
            if status.desired_state != "running" {
                continue;
            }
            let resumable = self.store.resumable_services(&status.name)?;
            if resumable.is_empty() || resumable.len() != status.services.len() {
                continue;
            }
            let Some(existing) = self.store.existing_stack(&status.name)? else {
                continue;
            };
            if existing.source.is_empty() {
                continue;
            }
            let manifest = crate::manifest::parse(&existing.source)?;
            let request_id = generate_internal_request_id("resume", &status.name)?;
            let base = existing.manifest_base.to_str().ok_or_else(|| {
                Error::Conflict("persisted manifest base is not UTF-8".to_owned())
            })?;
            self.start_current_stack(&request_id, "resume", &manifest, base)?;
        }
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) {
        let statuses = match self.store.stack_statuses() {
            Ok(statuses) => statuses,
            Err(error) => {
                eprintln!(
                    "termux-stacks daemon: could not inspect persisted restart delays during shutdown: {error}"
                );
                Vec::new()
            }
        };
        let status_by_stack = statuses
            .iter()
            .map(|status| (status.name.as_str(), status))
            .collect::<BTreeMap<_, _>>();
        let mut stacks = self
            .children
            .keys()
            .map(|key| key.stack.clone())
            .collect::<BTreeSet<_>>();
        stacks.extend(self.uncertain_children.keys().map(|key| key.stack.clone()));
        stacks.extend(
            statuses
                .iter()
                .filter(|status| {
                    status.services.iter().any(|service| {
                        service.desired_state == "running"
                            && service.observed_state == "restarting"
                            && service.effect_phase == "backoff"
                            && service.next_restart_at.is_some()
                    })
                })
                .map(|status| status.name.clone()),
        );
        for stack in stacks {
            let mut services = self
                .committed_manifest(&stack)
                .ok()
                .flatten()
                .map(|manifest| manifest.stop_order())
                .unwrap_or_default();
            let mut missing = self
                .children
                .keys()
                .filter(|key| key.stack == stack && !services.contains(&key.service))
                .map(|key| key.service.clone())
                .collect::<Vec<_>>();
            missing.extend(
                self.uncertain_children
                    .keys()
                    .filter(|key| key.stack == stack && !services.contains(&key.service))
                    .map(|key| key.service.clone()),
            );
            if let Some(status) = status_by_stack.get(stack.as_str()) {
                missing.extend(
                    status
                        .services
                        .iter()
                        .filter(|service| !services.contains(&service.name))
                        .map(|service| service.name.clone()),
                );
            }
            missing.sort();
            missing.dedup();
            missing.reverse();
            services.extend(missing);

            for service in services {
                let key = ServiceKey::new(&stack, service);
                if self.uncertain_children.contains_key(&key) {
                    let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                    eprintln!(
                        "termux-stacks daemon: halting shutdown of stack {:?} at unqualified service {:?}",
                        key.stack, key.service
                    );
                    break;
                }
                if !self.children.contains_key(&key) {
                    let persisted = status_by_stack.get(stack.as_str()).and_then(|status| {
                        status
                            .services
                            .iter()
                            .find(|candidate| candidate.name == key.service)
                    });
                    if persisted.is_some_and(|service| {
                        service.desired_state == "running"
                            && service.observed_state == "restarting"
                            && service.effect_phase == "backoff"
                            && service.next_restart_at.is_some()
                    }) {
                        if let Err(error) = self
                            .store
                            .record_graceful_backoff_stop(&key.stack, &key.service)
                        {
                            let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                            eprintln!(
                                "termux-stacks daemon: could not cancel restart delay for {:?}/{:?}: {error}",
                                key.stack, key.service
                            );
                            break;
                        }
                    } else if persisted.is_some_and(|service| {
                        matches!(
                            service.observed_state.as_str(),
                            "preparing" | "starting" | "running" | "stopping" | "restarting"
                        )
                    }) {
                        let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                        eprintln!(
                            "termux-stacks daemon: halting shutdown of stack {:?}; active service {:?} is not owned",
                            key.stack, key.service
                        );
                        break;
                    }
                    continue;
                }
                match self.stop_owned_child(&key) {
                    Ok(status) => {
                        let (code, signal) = exit_parts(&status);
                        if let Err(error) =
                            self.store
                                .record_graceful_stop(&key.stack, &key.service, code, signal)
                        {
                            let _ = self.store.mark_runtime_unknown(&key.stack, &key.service);
                            eprintln!(
                                "termux-stacks daemon: could not record graceful stop for {:?}/{:?}: {error}",
                                key.stack, key.service
                            );
                            break;
                        }
                    }
                    Err(error) => {
                        self.quarantine_managed_child(&key);
                        eprintln!(
                            "termux-stacks daemon: could not stop {:?}/{:?} during shutdown: {error}",
                            key.stack, key.service
                        );
                        break;
                    }
                }
            }
        }
        for key in self.uncertain_children.keys() {
            eprintln!(
                "termux-stacks daemon: preserving unqualified child handle {:?}/{:?}; state remains unknown",
                key.stack, key.service
            );
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.children.is_empty() || !self.uncertain_children.is_empty() {
            self.shutdown();
        }
    }
}

#[derive(Clone, Copy)]
enum StartJournal<'a> {
    Operation(&'a str),
    Restart,
}

fn create_log(path: &Path) -> Result<fs::File, Error> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(Error::UnsafeLog(path.to_path_buf()));
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::UnsafeLog(path.to_path_buf()));
    }
    Ok(file)
}

fn open_log_append(path: &Path) -> Result<fs::File, Error> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.permissions().mode() & 0o077 != 0
    {
        return Err(Error::UnsafeLog(path.to_path_buf()));
    }
    let file = OpenOptions::new().append(true).open(path)?;
    let after = file.metadata()?;
    if !after.is_file() || before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(Error::UnsafeLog(path.to_path_buf()));
    }
    Ok(file)
}

fn open_logs(stdout_path: &Path, stderr_path: &Path) -> Result<(fs::File, fs::File), Error> {
    Ok((open_log_append(stdout_path)?, open_log_append(stderr_path)?))
}

fn open_or_create_log(path: &Path) -> Result<fs::File, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => open_log_append(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_log(path),
        Err(error) => Err(Error::Io(error)),
    }
}

fn prepare_existing_logs(
    paths: &RuntimePaths,
    stack: &str,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<(fs::File, fs::File), Error> {
    paths.prepare_stack_log_directory(stack)?;
    Ok((
        open_or_create_log(stdout_path)?,
        open_or_create_log(stderr_path)?,
    ))
}

fn replay_matches(
    replay: &crate::store::OperationReplay,
    operation: &str,
    stack: &str,
    candidate_manifest: Option<&str>,
    manifest_base: Option<&str>,
    target_service: Option<&str>,
) -> bool {
    replay.operation == operation
        && replay.stack_name == stack
        && replay.candidate_manifest.as_deref() == candidate_manifest
        && replay.manifest_base.as_deref() == manifest_base
        && replay.target_service.as_deref() == target_service
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

fn generate_internal_request_id(operation: &str, stack: &str) -> Result<String, Error> {
    let mut random = [0_u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "internal-{operation}-{}-{suffix}",
        short_name(stack)
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

fn exit_parts(status: &ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), status.signal())
}

fn should_restart(policy: RestartPolicy, status: &ExitStatus) -> bool {
    match policy {
        RestartPolicy::No => false,
        RestartPolicy::OnFailure => !status.success(),
        RestartPolicy::Always => true,
    }
}

fn should_restart_parts(policy: RestartPolicy, code: Option<i32>, signal: Option<i32>) -> bool {
    if signal.is_some() {
        return false;
    }
    match policy {
        RestartPolicy::No => false,
        RestartPolicy::OnFailure => code.is_some_and(|code| code != 0),
        RestartPolicy::Always => code.is_some(),
    }
}

fn restart_delay_seconds(previous_attempts: u32) -> u64 {
    #[cfg(debug_assertions)]
    if std::env::var_os("TERMUX_STACKS_TEST_IMMEDIATE_RESTART").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        return 0;
    }

    RESTART_DELAYS[usize::try_from(previous_attempts).expect("u32 fits usize")]
}

fn unix_time() -> Result<i64, Error> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| io::Error::other(format!("system clock before Unix epoch: {error}")))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::Io(io::Error::other("system time overflow")))
}

#[cfg(debug_assertions)]
fn fault_checkpoint(name: &str) -> Result<(), Error> {
    let Some(directory) = std::env::var_os("TERMUX_STACKS_FAULT_DIR") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    if !directory.is_absolute() {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TERMUX_STACKS_FAULT_DIR must be absolute",
        )));
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TERMUX_STACKS_FAULT_DIR must be a private real directory",
        )));
    }
    let reached = directory.join(format!("{name}.reached"));
    let proceed = directory.join(format!("{name}.continue"));
    let fail = directory.join(format!("{name}.fail"));
    if !reached.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&reached)?;
    }
    if fail.is_file() {
        return Err(Error::Io(io::Error::other(format!(
            "fault checkpoint {name:?} injected a failure"
        ))));
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while !proceed.is_file() {
        if Instant::now() >= deadline {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("fault checkpoint {name:?} timed out"),
            )));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[cfg(not(debug_assertions))]
fn fault_checkpoint(_name: &str) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{replay_matches, should_restart_parts};
    use crate::manifest::RestartPolicy;
    use crate::store::OperationReplay;

    #[test]
    fn replay_identity_includes_the_complete_mutation_payload() {
        let replay = OperationReplay {
            operation: "up".into(),
            stack_name: "demo".into(),
            candidate_manifest: Some("manifest-v1".into()),
            manifest_base: Some("/project".into()),
            target_service: None,
            response_json: None,
        };
        assert!(replay_matches(
            &replay,
            "up",
            "demo",
            Some("manifest-v1"),
            Some("/project"),
            None,
        ));
        assert!(!replay_matches(
            &replay,
            "up",
            "demo",
            Some("manifest-v2"),
            Some("/project"),
            None,
        ));
        assert!(!replay_matches(
            &replay,
            "up",
            "demo",
            Some("manifest-v1"),
            Some("/other"),
            None,
        ));

        let restart = OperationReplay {
            operation: "restart".into(),
            stack_name: "demo".into(),
            candidate_manifest: None,
            manifest_base: None,
            target_service: Some("api".into()),
            response_json: None,
        };
        assert!(replay_matches(
            &restart,
            "restart",
            "demo",
            None,
            None,
            Some("api"),
        ));
        assert!(!replay_matches(
            &restart,
            "restart",
            "demo",
            None,
            None,
            Some("web"),
        ));
    }

    #[test]
    fn a_tracer_signal_never_authorizes_an_automatic_restart() {
        assert!(!should_restart_parts(
            RestartPolicy::OnFailure,
            None,
            Some(9),
        ));
        assert!(!should_restart_parts(RestartPolicy::Always, None, Some(9),));
        assert!(should_restart_parts(
            RestartPolicy::OnFailure,
            Some(17),
            None,
        ));
    }
}
