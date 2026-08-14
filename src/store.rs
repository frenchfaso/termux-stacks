use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 3;

pub(crate) struct Store {
    connection: Connection,
    installation_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StackStatus {
    pub(crate) name: String,
    pub(crate) desired_state: String,
    pub(crate) observed_state: String,
    pub(crate) revision: i64,
    pub(crate) services: Vec<ServiceStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ServiceStatus {
    pub(crate) name: String,
    pub(crate) desired_state: String,
    pub(crate) observed_state: String,
    pub(crate) effect_phase: String,
    pub(crate) rootfs_state: String,
    pub(crate) alias: Option<String>,
    pub(crate) generation: Option<i64>,
    pub(crate) session_id: Option<i64>,
    pub(crate) last_exit_code: Option<i64>,
    pub(crate) last_exit_signal: Option<i64>,
    pub(crate) restart_attempts: i64,
    pub(crate) next_restart_at: Option<i64>,
    pub(crate) stdout_log: String,
    pub(crate) stderr_log: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ServiceIdentity {
    pub(crate) alias: String,
    pub(crate) generation: i64,
    pub(crate) session_id: Option<u32>,
    pub(crate) child_pid: Option<u32>,
    pub(crate) child_starttime: Option<i64>,
    pub(crate) boot_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationReplay {
    pub(crate) operation: String,
    pub(crate) stack_name: String,
    pub(crate) candidate_manifest: Option<String>,
    pub(crate) manifest_base: Option<String>,
    pub(crate) target_service: Option<String>,
    pub(crate) response_json: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExistingStack {
    pub(crate) source: String,
    pub(crate) manifest_base: PathBuf,
    pub(crate) revision: i64,
    pub(crate) desired_state: String,
    pub(crate) observed_state: String,
    pub(crate) services: Vec<ExistingService>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExistingService {
    pub(crate) name: String,
    pub(crate) config_json: String,
    pub(crate) desired_state: String,
    pub(crate) observed_state: String,
    pub(crate) effect_phase: String,
    pub(crate) alias: Option<String>,
    pub(crate) rootfs_state: String,
    pub(crate) restart_attempts: i64,
    pub(crate) restart_window_started_at: Option<i64>,
    pub(crate) next_restart_at: Option<i64>,
    pub(crate) stdout_log: PathBuf,
    pub(crate) stderr_log: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ServicePlan {
    pub(crate) alias: String,
    pub(crate) stdout_log: PathBuf,
    pub(crate) stderr_log: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct CandidateRecovery {
    pub(crate) revision: i64,
    pub(crate) services: BTreeMap<String, CandidateRecoveryService>,
}

#[derive(Clone, Debug)]
pub(crate) struct CandidateRecoveryService {
    pub(crate) plan: ServicePlan,
    pub(crate) reuse_installed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduledRestart {
    pub(crate) stack_name: String,
    pub(crate) service_name: String,
    pub(crate) at: i64,
    pub(crate) attempts: i64,
}

#[derive(Debug)]
pub(crate) enum Error {
    UnsafePath(PathBuf),
    Io(io::Error),
    Sql(rusqlite::Error),
    Schema(i64),
    Conflict(String),
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafePath(path) => write!(formatter, "unsafe database path {}", path.display()),
            Self::Io(error) => write!(formatter, "database filesystem error: {error}"),
            Self::Sql(error) => write!(formatter, "database error: {error}"),
            Self::Schema(version) => write!(
                formatter,
                "unsupported database schema version {version}; expected {SCHEMA_VERSION}"
            ),
            Self::Conflict(message) => formatter.write_str(message),
            Self::NotFound(subject) => write!(formatter, "{subject} does not exist"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl Store {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        prepare_database_file(path)?;
        let connection = Connection::open(path)?;
        let version =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
        if !matches!(version, 0 | 2 | SCHEMA_VERSION) {
            return Err(Error::Schema(version));
        }

        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;",
        )?;
        match version {
            0 => initialize_schema(&connection)?,
            2 => migrate_v2_to_v3(&connection)?,
            SCHEMA_VERSION => {}
            _ => unreachable!("supported schema versions were checked"),
        }
        apply_debug_storage_limit(&connection)?;

        let installation_id = connection.query_row(
            "SELECT value FROM meta WHERE key = 'installation_id'",
            [],
            |row| row.get(0),
        )?;
        Ok(Self {
            connection,
            installation_id,
        })
    }

    pub(crate) fn installation_id(&self) -> &str {
        &self.installation_id
    }

    pub(crate) fn stack_status(&self, name: &str) -> Result<Option<StackStatus>, Error> {
        let stack = self
            .connection
            .query_row(
                "SELECT name, desired_state, observed_state, committed_revision
                   FROM stacks WHERE name = ?1",
                [name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((name, desired_state, observed_state, revision)) = stack else {
            return Ok(None);
        };

        let mut statement = self.connection.prepare(
            "SELECT v.name, v.desired_state, v.observed_state, v.effect_phase,
                    COALESCE(candidate.state, current.state, 'absent'),
                    COALESCE(candidate.alias, v.current_alias),
                    COALESCE(candidate.generation, v.current_generation),
                    v.session_id, v.last_exit_code, v.last_exit_signal,
                    v.restart_attempts, v.next_restart_at,
                    v.stdout_log_path, v.stderr_log_path
               FROM services AS v
               LEFT JOIN rootfs_generations AS current
                 ON current.stack_name = v.stack_name
                AND current.service_name = v.name
                AND current.generation = v.current_generation
               LEFT JOIN rootfs_generations AS candidate
                 ON candidate.stack_name = v.stack_name
                AND candidate.service_name = v.name
                AND candidate.role = 'candidate'
                AND EXISTS (
                    SELECT 1
                      FROM operation_services AS os
                      JOIN operations AS o ON o.request_id = os.request_id
                     WHERE o.stack_name = v.stack_name
                       AND os.service_name = v.name
                       AND os.generation = candidate.generation
                       AND o.operation = 'up'
                       AND (
                           o.outcome IS NULL
                           OR (?2 = 0 AND o.outcome = 'failure')
                       )
                )
              WHERE v.stack_name = ?1 AND v.active = 1
              ORDER BY v.name",
        )?;
        let services = statement
            .query_map(params![&name, revision], |row| {
                Ok(ServiceStatus {
                    name: row.get(0)?,
                    desired_state: row.get(1)?,
                    observed_state: row.get(2)?,
                    effect_phase: row.get(3)?,
                    rootfs_state: row.get(4)?,
                    alias: row.get(5)?,
                    generation: row.get(6)?,
                    session_id: row.get(7)?,
                    last_exit_code: row.get(8)?,
                    last_exit_signal: row.get(9)?,
                    restart_attempts: row.get(10)?,
                    next_restart_at: row.get(11)?,
                    stdout_log: row.get(12)?,
                    stderr_log: row.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(StackStatus {
            name,
            desired_state,
            observed_state,
            revision,
            services,
        }))
    }

    pub(crate) fn stack_statuses(&self) -> Result<Vec<StackStatus>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM stacks ORDER BY name")?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        names
            .iter()
            .map(|name| {
                self.stack_status(name)?
                    .ok_or_else(|| Error::NotFound(format!("stack {name:?}")))
            })
            .collect()
    }

    /// Durably records a complete candidate before any filesystem or engine effect.
    /// Committed stacks must be stopped and keep the same service-name set in M1.
    /// A never-committed, terminal-safe candidate may replace its logical set;
    /// obsolete services are soft-retired so generation ownership is retained.
    pub(crate) fn begin_up(
        &mut self,
        request_id: &str,
        source: &str,
        manifest_base: &Path,
        manifest: &crate::manifest::Manifest,
        plans: &BTreeMap<String, ServicePlan>,
    ) -> Result<i64, Error> {
        let manifest_names = manifest.services.keys().cloned().collect::<BTreeSet<_>>();
        let plan_names = plans.keys().cloned().collect::<BTreeSet<_>>();
        if manifest_names != plan_names {
            return Err(Error::Conflict(
                "service plans must match the manifest service set exactly".into(),
            ));
        }

        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        reject_unfinished_operation(&transaction, &manifest.name)?;

        let existing = transaction
            .query_row(
                "SELECT committed_revision FROM stacks WHERE name = ?1",
                [&manifest.name],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let existing_revision = existing.unwrap_or(0);
        let revision = existing_revision + 1;

        if existing.is_some() {
            let current_names = service_names(&transaction, &manifest.name)?;
            if existing_revision > 0 && current_names != manifest_names {
                return Err(Error::Conflict(
                    "adding or removing services during update is deferred; service names must match the committed revision"
                        .into(),
                ));
            }
            let unsafe_services: i64 = transaction.query_row(
                "SELECT count(*) FROM services
                  WHERE stack_name = ?1 AND active = 1
                    AND (observed_state NOT IN ('stopped', 'failed')
                         OR effect_phase = 'unknown'
                         OR child_pid IS NOT NULL OR session_id IS NOT NULL
                         OR child_starttime IS NOT NULL OR boot_id IS NOT NULL
                         OR next_restart_at IS NOT NULL)",
                [&manifest.name],
                |row| row.get(0),
            )?;
            if unsafe_services != 0 {
                return Err(Error::Conflict(format!(
                    "stack {:?} must be fully stopped before preparing a new revision",
                    manifest.name
                )));
            }
            if existing_revision == 0
                && !revision_zero_candidate_allows_fresh_up(
                    &transaction,
                    &manifest.name,
                    &current_names,
                )?
            {
                return Err(Error::Conflict(format!(
                    "stack {:?} has a never-committed candidate that is structurally incomplete, unknown, or may have invoked an engine effect",
                    manifest.name
                )));
            }
            if existing_revision == 0 {
                for obsolete in current_names.difference(&manifest_names) {
                    transaction.execute(
                        "UPDATE rootfs_generations
                            SET role = 'retired', updated_at = ?3
                          WHERE stack_name = ?1 AND service_name = ?2
                            AND role = 'candidate'",
                        params![manifest.name, obsolete, now],
                    )?;
                    require_one(
                        transaction.execute(
                            "UPDATE services
                                SET active = 0, desired_state = 'stopped',
                                    next_restart_at = NULL, updated_at = ?3
                              WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                            params![manifest.name, obsolete, now],
                        )?,
                        format!("active service {:?}/{obsolete:?}", manifest.name),
                    )?;
                }
            }
            transaction.execute(
                "UPDATE stacks
                    SET desired_state = 'running', observed_state = 'starting', updated_at = ?2
                  WHERE name = ?1",
                params![manifest.name, now],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO stacks(
                     name, desired_state, observed_state, manifest, manifest_base,
                     committed_revision, created_at, updated_at
                 ) VALUES (?1, 'running', 'starting', '', '', 0, ?2, ?2)",
                params![manifest.name, now],
            )?;
        }

        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 created_at, updated_at
             ) VALUES (?1, ?2, 'up', 'intent', ?3, ?4, ?5, NULL, ?6, ?6)",
            params![
                request_id,
                manifest.name,
                source,
                manifest_base.to_string_lossy(),
                revision,
                now
            ],
        )?;

        let order = manifest.start_order();
        for (ordinal, service_name) in order.iter().enumerate() {
            let service = manifest
                .services
                .get(service_name)
                .expect("start order contains a manifest service");
            let plan = plans
                .get(service_name)
                .expect("service plan set was validated");
            let config_json = service_config_json(service);

            transaction.execute(
                "UPDATE rootfs_generations
                    SET role = 'retired', updated_at = ?3
                  WHERE stack_name = ?1 AND service_name = ?2 AND role = 'candidate'",
                params![manifest.name, service_name, now],
            )?;
            let generation: i64 = transaction.query_row(
                "SELECT COALESCE(max(generation), 0) + 1
                   FROM rootfs_generations
                  WHERE stack_name = ?1 AND service_name = ?2",
                params![manifest.name, service_name],
                |row| row.get(0),
            )?;

            let service_exists: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM services WHERE stack_name = ?1 AND name = ?2
                 )",
                params![manifest.name, service_name],
                |row| row.get(0),
            )?;
            if service_exists {
                require_one(
                    transaction.execute(
                        "UPDATE services
                            SET active = 1, desired_state = 'running', observed_state = 'preparing',
                                effect_phase = 'intent', session_id = NULL, child_pid = NULL,
                                child_starttime = NULL, boot_id = NULL, last_exit_code = NULL,
                                last_exit_signal = NULL, restart_attempts = 0,
                                restart_window_started_at = NULL, next_restart_at = NULL,
                                stdout_log_path = ?3, stderr_log_path = ?4, updated_at = ?5
                          WHERE stack_name = ?1 AND name = ?2",
                        params![
                            manifest.name,
                            service_name,
                            plan.stdout_log.to_string_lossy(),
                            plan.stderr_log.to_string_lossy(),
                            now
                        ],
                    )?,
                    format!("service {:?}/{service_name:?}", manifest.name),
                )?;
            } else {
                transaction.execute(
                    "INSERT INTO services(
                         stack_name, name, active, config_json, desired_state, observed_state,
                         current_alias, current_generation, effect_phase,
                         session_id, child_pid, child_starttime, boot_id,
                         last_exit_code, last_exit_signal, restart_attempts,
                         restart_window_started_at, next_restart_at,
                         stdout_log_path, stderr_log_path, created_at, updated_at
                     ) VALUES (
                         ?1, ?2, 1, ?3, 'running', 'preparing', NULL, NULL, 'intent',
                         NULL, NULL, NULL, NULL, NULL, NULL, 0, NULL, NULL,
                         ?4, ?5, ?6, ?6
                     )",
                    params![
                        manifest.name,
                        service_name,
                        config_json,
                        plan.stdout_log.to_string_lossy(),
                        plan.stderr_log.to_string_lossy(),
                        now
                    ],
                )?;
            }

            transaction.execute(
                "INSERT INTO rootfs_generations(
                     stack_name, service_name, generation, alias, image, state, role,
                     created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'preparing', 'candidate', ?6, ?6)",
                params![
                    manifest.name,
                    service_name,
                    generation,
                    plan.alias,
                    service.image,
                    now
                ],
            )?;
            transaction.execute(
                "INSERT INTO operation_services(
                     request_id, stack_name, service_name, ordinal, phase, outcome,
                     candidate_config_json, generation, error_code, error_message
                 ) VALUES (?1, ?2, ?3, ?4, 'intent', NULL, ?5, ?6, NULL, NULL)",
                params![
                    request_id,
                    manifest.name,
                    service_name,
                    i64::try_from(ordinal).expect("manifest service limit fits in i64"),
                    config_json,
                    generation
                ],
            )?;
        }
        transaction.commit()?;
        Ok(revision)
    }

    /// Starts a fresh `up` journal from one exact failed candidate revision.
    /// Installed candidates are reused without an engine effect. Candidates
    /// that are durably absent or failed before an install invocation receive
    /// a fresh generation and a never-persisted alias from `fresh_plans`.
    pub(crate) fn begin_candidate_recovery(
        &mut self,
        request_id: &str,
        source: &str,
        manifest_base: &Path,
        manifest: &crate::manifest::Manifest,
        fresh_plans: &BTreeMap<String, ServicePlan>,
    ) -> Result<CandidateRecovery, Error> {
        let manifest_names = manifest.services.keys().cloned().collect::<BTreeSet<_>>();
        let plan_names = fresh_plans.keys().cloned().collect::<BTreeSet<_>>();
        if manifest_names != plan_names {
            return Err(Error::Conflict(
                "candidate recovery plans must match the manifest service set exactly".into(),
            ));
        }
        let aliases = fresh_plans
            .values()
            .map(|plan| plan.alias.as_str())
            .collect::<BTreeSet<_>>();
        if aliases.len() != fresh_plans.len() {
            return Err(Error::Conflict(
                "candidate recovery plans contain duplicate aliases".into(),
            ));
        }

        let now = unix_time()?;
        let base = manifest_base.to_string_lossy().into_owned();
        let transaction = self.connection.transaction()?;
        reject_unfinished_operation(&transaction, &manifest.name)?;

        let committed_revision: i64 = transaction
            .query_row(
                "SELECT committed_revision FROM stacks WHERE name = ?1",
                [&manifest.name],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("stack {:?}", manifest.name)))?;
        let current_names = service_names(&transaction, &manifest.name)?;
        if committed_revision > 0 && (current_names != manifest_names || current_names.is_empty()) {
            return Err(Error::Conflict(
                "candidate recovery requires the exact logical service set".into(),
            ));
        }
        let unsafe_services: i64 = transaction.query_row(
            "SELECT count(*) FROM services
              WHERE stack_name = ?1 AND active = 1
                AND (observed_state NOT IN ('failed', 'stopped')
                     OR effect_phase = 'unknown'
                     OR session_id IS NOT NULL OR child_pid IS NOT NULL
                     OR child_starttime IS NOT NULL OR boot_id IS NOT NULL
                     OR next_restart_at IS NOT NULL)",
            [&manifest.name],
            |row| row.get(0),
        )?;
        if unsafe_services != 0 {
            return Err(Error::Conflict(format!(
                "candidate recovery refused: {unsafe_services} service(s) have identity, unknown/active state, or pending restart"
            )));
        }

        let expected_revision = committed_revision + 1;
        let mut statement = transaction.prepare(
            "SELECT request_id, candidate_manifest, manifest_base
               FROM operations
              WHERE stack_name = ?1 AND operation = 'up' AND outcome = 'failure'
                AND candidate_revision = ?2
              ORDER BY request_id",
        )?;
        let failed_candidate_operations = statement
            .query_map(params![manifest.name, expected_revision], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if failed_candidate_operations.is_empty() {
            return Err(Error::NotFound(format!(
                "recoverable candidate for stack {:?}",
                manifest.name
            )));
        }
        let mut terminal_safe_candidate = false;
        for (previous, _, _) in &failed_candidate_operations {
            if terminal_candidate_allows_fresh_up(
                &transaction,
                &manifest.name,
                &current_names,
                previous,
            )? {
                terminal_safe_candidate = true;
                break;
            }
        }
        if current_names != manifest_names {
            if committed_revision == 0 && terminal_safe_candidate {
                return Err(Error::NotFound(format!(
                    "recoverable candidate for stack {:?}",
                    manifest.name
                )));
            }
            return Err(Error::Conflict(
                "candidate recovery refused: changing the logical service set requires a terminal-safe, never-committed candidate"
                    .into(),
            ));
        }
        let prior_operations = failed_candidate_operations
            .iter()
            .filter(|(_, candidate_manifest, candidate_base)| {
                candidate_manifest.as_deref() == Some(source)
                    && candidate_base.as_deref() == Some(base.as_str())
            })
            .map(|(request_id, _, _)| request_id.as_str())
            .collect::<Vec<_>>();
        if prior_operations.is_empty() {
            if terminal_safe_candidate {
                return Err(Error::NotFound(format!(
                    "recoverable candidate for stack {:?}",
                    manifest.name
                )));
            }
            return Err(Error::Conflict(
                "candidate recovery refused: the latest failed candidate is structurally incomplete, unknown, or may have invoked an engine effect"
                    .into(),
            ));
        }
        let config_map = manifest
            .services
            .iter()
            .map(|(name, service)| (name.clone(), service_config_json(service)))
            .collect::<BTreeMap<_, _>>();
        let mut previous_rows = None;
        let mut exact_structure = false;
        for previous in prior_operations {
            let rows = operation_rows(&transaction, previous)?;
            if rows.iter().any(|row| row.stack_name != manifest.name) {
                continue;
            }
            let names = rows
                .iter()
                .map(|row| row.service_name.clone())
                .collect::<BTreeSet<_>>();
            let configs = rows
                .iter()
                .filter_map(|row| {
                    row.candidate_config_json
                        .as_ref()
                        .map(|config| (row.service_name.clone(), config.clone()))
                })
                .collect::<BTreeMap<_, _>>();
            let structural = names == manifest_names
                && configs == config_map
                && rows.iter().all(|row| row.generation.is_some());
            exact_structure |= structural;
            if structural
                && terminal_candidate_allows_fresh_up(
                    &transaction,
                    &manifest.name,
                    &manifest_names,
                    previous,
                )?
            {
                previous_rows = Some(rows);
                break;
            }
        }
        let Some(previous_rows) = previous_rows else {
            if exact_structure && terminal_safe_candidate {
                return Err(Error::NotFound(format!(
                    "recoverable candidate for stack {:?}",
                    manifest.name
                )));
            }
            return Err(Error::Conflict(
                "candidate generations do not belong to a failed up with the exact manifest, base, and service configuration"
                    .into(),
            ));
        };

        let previous_by_name = previous_rows
            .into_iter()
            .map(|row| (row.service_name.clone(), row))
            .collect::<BTreeMap<_, _>>();
        let mut candidates = BTreeMap::new();
        for service_name in &manifest_names {
            let row = previous_by_name
                .get(service_name)
                .expect("failed operation service set was checked");
            let generation = row
                .generation
                .expect("failed operation generations were checked");
            let candidate =
                recovery_generation(&transaction, &manifest.name, service_name, generation)?;
            let latest: i64 = transaction.query_row(
                "SELECT max(generation) FROM rootfs_generations
                  WHERE stack_name = ?1 AND service_name = ?2",
                params![manifest.name, service_name],
                |result| result.get(0),
            )?;
            let service = manifest
                .services
                .get(service_name)
                .expect("manifest service set was checked");
            if generation != latest || candidate.image != service.image {
                return Err(Error::Conflict(format!(
                    "candidate recovery refused for service {:?}/{service_name:?}: generation or image no longer matches",
                    manifest.name
                )));
            }
            let reuse_installed = candidate.state == "installed" && candidate.role == "candidate";
            let replace_pre_effect = (candidate.state == "absent"
                && matches!(candidate.role.as_str(), "candidate" | "retired"))
                || (candidate.state == "preparing"
                    && candidate.role == "candidate"
                    && row.phase == "failed_pre_effect");
            if !reuse_installed && !replace_pre_effect {
                return Err(Error::Conflict(format!(
                    "candidate recovery refused for service {:?}/{service_name:?}: rootfs state/role {:?}/{:?} is not proven reusable or pre-effect",
                    manifest.name, candidate.state, candidate.role
                )));
            }
            if replace_pre_effect {
                let plan = fresh_plans
                    .get(service_name)
                    .expect("recovery plan set was checked");
                let alias_exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM rootfs_generations WHERE alias = ?1)",
                    [&plan.alias],
                    |result| result.get(0),
                )?;
                if alias_exists {
                    return Err(Error::Conflict(format!(
                        "candidate recovery alias {:?} was already persisted and cannot be reused",
                        plan.alias
                    )));
                }
            }
            candidates.insert(service_name.clone(), (candidate, reuse_installed));
        }

        let all_installed = candidates.values().all(|(_, reuse)| *reuse);

        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 created_at, updated_at
             ) VALUES (?1, ?2, 'up', ?3, ?4, ?5, ?6, NULL, ?7, ?7)",
            params![
                request_id,
                manifest.name,
                if all_installed { "installed" } else { "intent" },
                source,
                base,
                expected_revision,
                now
            ],
        )?;
        require_one(
            transaction.execute(
                "UPDATE stacks
                    SET desired_state = 'running', observed_state = 'starting', updated_at = ?2
                  WHERE name = ?1",
                params![manifest.name, now],
            )?,
            format!("stack {:?}", manifest.name),
        )?;

        let mut services = BTreeMap::new();
        for (ordinal, service_name) in manifest.start_order().iter().enumerate() {
            let (candidate, reuse_installed) = candidates
                .get(service_name)
                .expect("candidate service set was checked");
            let config = config_map
                .get(service_name)
                .expect("manifest config set was checked");
            let (plan, generation, phase, observed) = if *reuse_installed {
                (
                    ServicePlan {
                        alias: candidate.alias.clone(),
                        stdout_log: candidate.stdout_log.clone(),
                        stderr_log: candidate.stderr_log.clone(),
                    },
                    candidate.generation,
                    "installed",
                    "starting",
                )
            } else {
                let plan = fresh_plans
                    .get(service_name)
                    .expect("recovery plan set was checked")
                    .clone();
                require_one(
                    transaction.execute(
                        "UPDATE rootfs_generations
                            SET state = 'absent', role = 'retired', updated_at = ?4
                          WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                        params![manifest.name, service_name, candidate.generation, now],
                    )?,
                    format!(
                        "previous candidate rootfs {:?}/{service_name:?}/{}",
                        manifest.name, candidate.generation
                    ),
                )?;
                let generation = candidate.generation + 1;
                let image = &manifest
                    .services
                    .get(service_name)
                    .expect("manifest service set was checked")
                    .image;
                transaction.execute(
                    "INSERT INTO rootfs_generations(
                         stack_name, service_name, generation, alias, image, state, role,
                         created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 'preparing', 'candidate', ?6, ?6)",
                    params![
                        manifest.name,
                        service_name,
                        generation,
                        plan.alias,
                        image,
                        now
                    ],
                )?;
                (plan, generation, "intent", "preparing")
            };
            require_one(
                transaction.execute(
                    "UPDATE services
                        SET desired_state = 'running', observed_state = ?3,
                            effect_phase = ?4, session_id = NULL, child_pid = NULL,
                            child_starttime = NULL, boot_id = NULL,
                            last_exit_code = NULL, last_exit_signal = NULL,
                            restart_attempts = 0, restart_window_started_at = NULL,
                            next_restart_at = NULL, stdout_log_path = ?5,
                            stderr_log_path = ?6, updated_at = ?7
                      WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                    params![
                        manifest.name,
                        service_name,
                        observed,
                        phase,
                        plan.stdout_log.to_string_lossy(),
                        plan.stderr_log.to_string_lossy(),
                        now
                    ],
                )?,
                format!("service {:?}/{service_name:?}", manifest.name),
            )?;
            transaction.execute(
                "INSERT INTO operation_services(
                     request_id, stack_name, service_name, ordinal, phase, outcome,
                     candidate_config_json, generation, error_code, error_message
                 ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, NULL, NULL)",
                params![
                    request_id,
                    manifest.name,
                    service_name,
                    i64::try_from(ordinal).expect("manifest service limit fits in i64"),
                    phase,
                    config,
                    generation
                ],
            )?;
            services.insert(
                service_name.clone(),
                CandidateRecoveryService {
                    plan,
                    reuse_installed: *reuse_installed,
                },
            );
        }
        transaction.commit()?;
        Ok(CandidateRecovery {
            revision: expected_revision,
            services,
        })
    }

    pub(crate) fn mark_logs_prepared(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        self.set_candidate_phase(request_id, stack, service, "logs_prepared")
    }

    pub(crate) fn mark_install_invoked(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        self.set_candidate_phase(request_id, stack, service, "install_invoked")
    }

    pub(crate) fn mark_installed(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let generation = operation_generation(&transaction, request_id, stack, service)?;
        require_one(
            transaction.execute(
                "UPDATE rootfs_generations
                    SET state = 'installed', updated_at = ?4
                  WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                params![stack, service, generation, now],
            )?,
            format!("rootfs generation {stack:?}/{service:?}/{generation}"),
        )?;
        set_service_phase(&transaction, request_id, stack, service, "installed", now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_rootfs_unknown(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let generation = operation_generation(&transaction, request_id, stack, service)?;
        require_one(
            transaction.execute(
                "UPDATE rootfs_generations SET state = 'unknown', updated_at = ?4
                  WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                params![stack, service, generation, now],
            )?,
            format!("rootfs generation {stack:?}/{service:?}/{generation}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_start_invoked(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        self.set_candidate_phase(request_id, stack, service, "start_invoked")
    }

    pub(crate) fn mark_starting(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        child_pid: u32,
        child_starttime: u64,
        boot_id: &str,
    ) -> Result<(), Error> {
        let child_starttime = i64::try_from(child_starttime)
            .map_err(|_| Error::Io(io::Error::other("process starttime exceeds SQLite INTEGER")))?;
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        set_service_phase(&transaction, request_id, stack, service, "starting", now)?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'starting', child_pid = ?3,
                        child_starttime = ?4, boot_id = ?5, updated_at = ?6
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, child_pid, child_starttime, boot_id, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_running(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        session_id: u32,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE operation_services
                    SET phase = 'running', outcome = 'success', error_code = NULL,
                        error_message = NULL
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3",
                params![request_id, stack, service],
            )?,
            format!("operation service {request_id:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'running', effect_phase = 'running',
                        session_id = ?3, restart_attempts = 0,
                        restart_window_started_at = NULL, next_restart_at = NULL,
                        updated_at = ?4
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, session_id, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_service_failed(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        code: &str,
        message: &str,
        unknown: bool,
    ) -> Result<(), Error> {
        let state = if unknown { "unknown" } else { "failed" };
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = ?3, effect_phase = ?3, updated_at = ?4
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, state, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE operation_services
                    SET phase = CASE
                            WHEN ?4 = 'unknown' THEN 'unknown'
                            WHEN phase IN ('intent', 'logs_prepared') THEN 'failed_pre_effect'
                            ELSE 'failed'
                        END,
                        outcome = 'failure', error_code = ?5, error_message = ?6
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3",
                params![request_id, stack, service, state, code, message],
            )?,
            format!("operation service {request_id:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Terminalizes a failed mutation only after the runtime has completed
    /// its exact-service cleanup. Until this call, candidate aliases remain
    /// selected by service_identity through the unfinished parent operation.
    pub(crate) fn finalize_operation_failure(
        &mut self,
        request_id: &str,
        stack: &str,
        code: &str,
        message: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        require_one(
            self.connection.execute(
                "UPDATE operations
                    SET phase = 'failed', outcome = 'failure', error_code = ?3,
                        error_message = ?4, updated_at = ?5
                  WHERE request_id = ?1 AND stack_name = ?2 AND outcome IS NULL
                    AND EXISTS (
                        SELECT 1 FROM operation_services
                         WHERE operation_services.request_id = operations.request_id
                           AND operation_services.outcome = 'failure'
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM operation_services
                         WHERE operation_services.request_id = operations.request_id
                           AND operation_services.outcome IS NULL
                    )",
                params![request_id, stack, code, message, now],
            )?,
            format!("failed unfinished operation {request_id:?}"),
        )
    }

    /// Makes every candidate service and rootfs pointer visible as one committed revision.
    pub(crate) fn commit_up(&mut self, request_id: &str, stack: &str) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let (manifest, manifest_base, revision): (String, String, i64) = transaction
            .query_row(
                "SELECT candidate_manifest, manifest_base, candidate_revision
                   FROM operations
                  WHERE request_id = ?1 AND stack_name = ?2 AND operation = 'up'
                    AND outcome IS NULL",
                params![request_id, stack],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("unfinished up operation {request_id:?}")))?;
        let incomplete: i64 = transaction.query_row(
            "SELECT count(*) FROM operation_services
              WHERE request_id = ?1 AND outcome IS NOT 'success'",
            [request_id],
            |row| row.get(0),
        )?;
        if incomplete != 0 {
            return Err(Error::Conflict(format!(
                "cannot commit operation {request_id:?}: {incomplete} service(s) are not running"
            )));
        }

        let rows = operation_rows(&transaction, request_id)?;
        for row in &rows {
            let generation = row.generation.ok_or_else(|| {
                Error::Conflict(format!(
                    "operation {request_id:?} service {:?} has no rootfs generation",
                    row.service_name
                ))
            })?;
            let config = row.candidate_config_json.as_ref().ok_or_else(|| {
                Error::Conflict(format!(
                    "operation {request_id:?} service {:?} has no candidate config",
                    row.service_name
                ))
            })?;
            let alias: String = transaction.query_row(
                "SELECT alias FROM rootfs_generations
                  WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3
                    AND state = 'installed' AND role = 'candidate'",
                params![stack, row.service_name, generation],
                |result| result.get(0),
            )?;
            transaction.execute(
                "UPDATE rootfs_generations SET role = 'retired', updated_at = ?3
                  WHERE stack_name = ?1 AND service_name = ?2 AND role = 'current'",
                params![stack, row.service_name, now],
            )?;
            require_one(
                transaction.execute(
                    "UPDATE rootfs_generations SET role = 'current', updated_at = ?4
                      WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                    params![stack, row.service_name, generation, now],
                )?,
                format!(
                    "rootfs generation {stack:?}/{:?}/{generation}",
                    row.service_name
                ),
            )?;
            require_one(
                transaction.execute(
                    "UPDATE services
                        SET config_json = ?3, current_alias = ?4, current_generation = ?5,
                            desired_state = 'running', observed_state = 'running',
                            effect_phase = 'committed', updated_at = ?6
                      WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                    params![stack, row.service_name, config, alias, generation, now],
                )?,
                format!("service {stack:?}/{:?}", row.service_name),
            )?;
        }
        require_one(
            transaction.execute(
                "UPDATE stacks
                    SET desired_state = 'running', observed_state = 'running', manifest = ?3,
                        manifest_base = ?4, committed_revision = ?5, updated_at = ?6
                  WHERE name = ?1 AND committed_revision < ?2",
                params![stack, revision, manifest, manifest_base, revision, now],
            )?,
            format!("stack {stack:?} revision {revision}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE operations
                    SET phase = 'committed', outcome = 'success', updated_at = ?2
                  WHERE request_id = ?1 AND outcome IS NULL",
                params![request_id, now],
            )?,
            format!("operation {request_id:?}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn begin_down(
        &mut self,
        request_id: &str,
        stack: &str,
        stop_order: &[String],
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        reject_unfinished_operation(&transaction, stack)?;
        let current_names = service_names(&transaction, stack)?;
        if current_names.is_empty() {
            return Err(Error::NotFound(format!("stack {stack:?}")));
        }
        let ordered_names = stop_order.iter().cloned().collect::<BTreeSet<_>>();
        if current_names != ordered_names || stop_order.len() != ordered_names.len() {
            return Err(Error::Conflict(
                "down order must contain every service exactly once".into(),
            ));
        }
        let revision: i64 = transaction.query_row(
            "SELECT committed_revision FROM stacks WHERE name = ?1",
            [stack],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 created_at, updated_at
             ) VALUES (?1, ?2, 'down', 'stop_intent', NULL, NULL, ?3, NULL, ?4, ?4)",
            params![request_id, stack, revision, now],
        )?;
        require_one(
            transaction.execute(
                "UPDATE stacks
                    SET desired_state = 'stopped', observed_state = 'stopping', updated_at = ?2
                  WHERE name = ?1",
                params![stack, now],
            )?,
            format!("stack {stack:?}"),
        )?;
        let mut has_unknown = false;
        for (ordinal, service) in stop_order.iter().enumerate() {
            let (observed, effect_phase, next_restart_at, session_id, child_pid): (
                String,
                String,
                Option<i64>,
                Option<i64>,
                Option<i64>,
            ) = transaction.query_row(
                "SELECT observed_state, effect_phase, next_restart_at, session_id, child_pid
                   FROM services WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
            let identity_absent = session_id.is_none() && child_pid.is_none();
            let proven_backoff = observed == "restarting"
                && effect_phase == "backoff"
                && next_restart_at.is_some()
                && identity_absent;
            let proven_absent = identity_absent
                && (matches!(observed.as_str(), "stopped" | "failed") || proven_backoff);
            let unknown = observed == "unknown"
                || (!proven_absent
                    && identity_absent
                    && matches!(
                        observed.as_str(),
                        "preparing" | "starting" | "running" | "stopping" | "restarting"
                    ));
            let (next_observed, next_phase, operation_phase, operation_outcome) = if proven_absent {
                ("stopped", "stopped", "stopped", Some("success"))
            } else if unknown {
                has_unknown = true;
                ("unknown", "unknown", "unknown", Some("failure"))
            } else {
                ("stopping", "stop_intent", "stop_intent", None)
            };
            require_one(
                transaction.execute(
                    "UPDATE services
                        SET desired_state = 'stopped', observed_state = ?3,
                            effect_phase = ?4,
                            next_restart_at = NULL, updated_at = ?5
                      WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                    params![stack, service, next_observed, next_phase, now],
                )?,
                format!("service {stack:?}/{service:?}"),
            )?;
            transaction.execute(
                "INSERT INTO operation_services(
                     request_id, stack_name, service_name, ordinal, phase, outcome,
                     candidate_config_json, generation, error_code, error_message
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8)",
                params![
                    request_id,
                    stack,
                    service,
                    i64::try_from(ordinal).expect("manifest service limit fits in i64"),
                    operation_phase,
                    operation_outcome,
                    unknown.then_some("unowned_unknown"),
                    unknown.then_some(
                        "service state is ambiguous; down did not signal an unowned process"
                    )
                ],
            )?;
        }
        refresh_stack_state(&transaction, stack, now)?;
        if has_unknown {
            require_one(
                transaction.execute(
                    "UPDATE operations
                        SET phase = 'unknown', outcome = 'failure',
                            error_code = 'unowned_unknown',
                            error_message = 'one or more services have ambiguous ownership; no unowned process was signalled',
                            updated_at = ?2
                      WHERE request_id = ?1",
                    params![request_id, now],
                )?,
                format!("down operation {request_id:?}"),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Journals a start against already committed rootfs generations. This is
    /// used both to resume a stopped stack and after a proven manual stop.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_start_current(
        &mut self,
        request_id: &str,
        stack: &str,
        operation: &str,
        ordered_services: &[String],
        target_service: Option<&str>,
        candidate_manifest: Option<&str>,
        manifest_base: Option<&Path>,
    ) -> Result<(), Error> {
        if ordered_services.is_empty() || operation.is_empty() {
            return Err(Error::Conflict(
                "a current-generation start needs an operation and at least one service".into(),
            ));
        }
        let unique = ordered_services.iter().cloned().collect::<BTreeSet<_>>();
        if unique.len() != ordered_services.len()
            || target_service
                .is_some_and(|target| ordered_services.len() != 1 || ordered_services[0] != target)
        {
            return Err(Error::Conflict(
                "current-generation start order contains duplicate services or does not match its target"
                    .into(),
            ));
        }
        if candidate_manifest.is_some() != manifest_base.is_some() {
            return Err(Error::Conflict(
                "current-generation replay metadata requires both manifest and base".into(),
            ));
        }

        let now = unix_time()?;
        let manifest_base = manifest_base.map(|path| path.to_string_lossy().into_owned());
        let transaction = self.connection.transaction()?;
        reject_unfinished_operation(&transaction, stack)?;
        let revision: i64 = transaction
            .query_row(
                "SELECT committed_revision FROM stacks WHERE name = ?1",
                [stack],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("stack {stack:?}")))?;
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'start_intent', ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                request_id,
                stack,
                operation,
                candidate_manifest,
                manifest_base,
                revision,
                target_service,
                now
            ],
        )?;

        for (ordinal, service) in ordered_services.iter().enumerate() {
            let (generation, rootfs_state): (i64, String) = transaction
                .query_row(
                    "SELECT v.current_generation, r.state
                       FROM services AS v
                       JOIN rootfs_generations AS r
                         ON r.stack_name = v.stack_name AND r.service_name = v.name
                        AND r.generation = v.current_generation AND r.role = 'current'
                      WHERE v.stack_name = ?1 AND v.name = ?2 AND v.active = 1",
                    params![stack, service],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| {
                    Error::NotFound(format!(
                        "current rootfs generation for service {stack:?}/{service:?}"
                    ))
                })?;
            if rootfs_state != "installed" {
                return Err(Error::Conflict(format!(
                    "current rootfs for service {stack:?}/{service:?} is {rootfs_state:?}, not installed"
                )));
            }
            require_one(
                transaction.execute(
                    "UPDATE services
                        SET desired_state = 'running', observed_state = 'starting',
                            effect_phase = 'start_intent', session_id = NULL,
                            child_pid = NULL, child_starttime = NULL, boot_id = NULL,
                            last_exit_code = NULL, last_exit_signal = NULL,
                            next_restart_at = NULL, updated_at = ?3
                      WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                    params![stack, service, now],
                )?,
                format!("service {stack:?}/{service:?}"),
            )?;
            transaction.execute(
                "INSERT INTO operation_services(
                     request_id, stack_name, service_name, ordinal, phase, outcome,
                     candidate_config_json, generation, error_code, error_message
                 ) VALUES (?1, ?2, ?3, ?4, 'start_intent', NULL,
                           NULL, ?5, NULL, NULL)",
                params![
                    request_id,
                    stack,
                    service,
                    i64::try_from(ordinal).expect("manifest service limit fits in i64"),
                    generation
                ],
            )?;
        }
        require_one(
            transaction.execute(
                "UPDATE stacks
                    SET desired_state = 'running', observed_state = 'starting', updated_at = ?2
                  WHERE name = ?1",
                params![stack, now],
            )?,
            format!("stack {stack:?}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Journals an exact-service manual restart before changing or signalling
    /// the owned child. The persisted identity remains available for the stop
    /// qualification performed by the runtime.
    pub(crate) fn begin_restart(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        reject_unfinished_operation(&transaction, stack)?;
        let (revision, generation, rootfs_state, observed_state, has_identity): (
            i64,
            i64,
            String,
            String,
            bool,
        ) = transaction
            .query_row(
                "SELECT s.committed_revision, v.current_generation, r.state,
                        v.observed_state,
                        v.session_id IS NOT NULL AND v.child_pid IS NOT NULL
                   FROM stacks AS s
                   JOIN services AS v ON v.stack_name = s.name
                   JOIN rootfs_generations AS r
                     ON r.stack_name = v.stack_name AND r.service_name = v.name
                    AND r.generation = v.current_generation AND r.role = 'current'
                  WHERE s.name = ?1 AND v.name = ?2 AND v.active = 1
                    AND v.desired_state = 'running'",
                params![stack, service],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("running service {stack:?}/{service:?}")))?;
        if rootfs_state != "installed" || observed_state != "running" || !has_identity {
            return Err(Error::Conflict(format!(
                "service {stack:?}/{service:?} is not a qualified running service on an installed current rootfs"
            )));
        }

        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 created_at, updated_at
             ) VALUES (?1, ?2, 'restart', 'restart_stop_intent', NULL, NULL,
                       ?3, ?4, ?5, ?5)",
            params![request_id, stack, revision, service, now],
        )?;
        transaction.execute(
            "INSERT INTO operation_services(
                 request_id, stack_name, service_name, ordinal, phase, outcome,
                 candidate_config_json, generation, error_code, error_message
             ) VALUES (?1, ?2, ?3, 0, 'restart_stop_intent', NULL,
                       NULL, ?4, NULL, NULL)",
            params![request_id, stack, service, generation],
        )?;
        require_one(
            transaction.execute(
                "UPDATE services SET effect_phase = 'restart_stop_intent', updated_at = ?3
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Persists stop invocation before the engine receives the signal. Child
    /// identity is intentionally retained until its exit is directly observed.
    pub(crate) fn mark_restart_stop_invoked(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE operation_services SET phase = 'restart_stop_invoked'
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3
                    AND phase = 'restart_stop_intent' AND outcome IS NULL",
                params![request_id, stack, service],
            )?,
            format!("restart stop intent {request_id:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'stopping', effect_phase = 'restart_stop_invoked',
                        updated_at = ?3
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND session_id IS NOT NULL AND child_pid IS NOT NULL",
                params![stack, service, now],
            )?,
            format!("owned service {stack:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE operations SET phase = 'restart_stop_invoked', updated_at = ?2
                  WHERE request_id = ?1 AND operation = 'restart' AND outcome IS NULL",
                params![request_id, now],
            )?,
            format!("restart operation {request_id:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Records qualified child absence while retaining desired=running. The
    /// same operation may then use mark_start_invoked/starting/running.
    pub(crate) fn mark_restart_stopped(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE operation_services SET phase = 'shutdown_proven'
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3
                    AND phase = 'restart_stop_invoked' AND outcome IS NULL",
                params![request_id, stack, service],
            )?,
            format!("invoked restart stop {request_id:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'stopped', effect_phase = 'shutdown_proven',
                        session_id = NULL, child_pid = NULL, child_starttime = NULL,
                        boot_id = NULL, last_exit_code = ?3, last_exit_signal = ?4,
                        next_restart_at = NULL, updated_at = ?5
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running'",
                params![stack, service, exit_code, exit_signal, now],
            )?,
            format!("running service {stack:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE operations SET phase = 'shutdown_proven', updated_at = ?2
                  WHERE request_id = ?1 AND operation = 'restart' AND outcome IS NULL",
                params![request_id, now],
            )?,
            format!("restart operation {request_id:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn finish_restart(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        let target: Option<String> = self
            .connection
            .query_row(
                "SELECT target_service FROM operations
                  WHERE request_id = ?1 AND stack_name = ?2
                    AND operation = 'restart' AND outcome IS NULL",
                params![request_id, stack],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if target.as_deref() != Some(service) {
            return Err(Error::NotFound(format!(
                "restart operation {request_id:?} for service {stack:?}/{service:?}"
            )));
        }
        self.finish_start_current(request_id, stack)
    }

    pub(crate) fn finish_start_current(
        &mut self,
        request_id: &str,
        stack: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let incomplete: i64 = transaction.query_row(
            "SELECT count(*) FROM operation_services
              WHERE request_id = ?1 AND outcome IS NOT 'success'",
            [request_id],
            |row| row.get(0),
        )?;
        if incomplete != 0 {
            return Err(Error::Conflict(format!(
                "cannot finish start operation {request_id:?}: {incomplete} service(s) are not running"
            )));
        }
        let rows = operation_rows(&transaction, request_id)?;
        if rows.is_empty() || rows.iter().any(|row| row.stack_name != stack) {
            return Err(Error::NotFound(format!(
                "start operation {request_id:?} for stack {stack:?}"
            )));
        }
        for row in rows {
            require_one(
                transaction.execute(
                    "UPDATE services SET effect_phase = 'committed', updated_at = ?3
                      WHERE stack_name = ?1 AND name = ?2 AND active = 1
                        AND observed_state = 'running'",
                    params![stack, row.service_name, now],
                )?,
                format!("running service {stack:?}/{:?}", row.service_name),
            )?;
        }
        require_one(
            transaction.execute(
                "UPDATE operations
                    SET phase = 'committed', outcome = 'success', updated_at = ?3
                  WHERE request_id = ?1 AND stack_name = ?2 AND outcome IS NULL",
                params![request_id, stack, now],
            )?,
            format!("operation {request_id:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_stop_invoked(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        self.set_candidate_phase(request_id, stack, service, "stop_invoked")
    }

    pub(crate) fn mark_stopped(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let safe_to_record: bool = transaction
            .query_row(
                "SELECT phase != 'unknown' AND outcome IS NOT 'failure'
                   FROM operation_services
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3",
                params![request_id, stack, service],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                Error::NotFound(format!("operation service {request_id:?}/{service:?}"))
            })?;
        if !safe_to_record {
            return Err(Error::Conflict(format!(
                "service {stack:?}/{service:?} has ambiguous ownership; stopped state cannot be recorded"
            )));
        }
        require_one(
            transaction.execute(
                "UPDATE services
                    SET desired_state = 'stopped', observed_state = 'stopped',
                        effect_phase = 'stopped', session_id = NULL, child_pid = NULL,
                        child_starttime = NULL, boot_id = NULL,
                        last_exit_code = ?3, last_exit_signal = ?4,
                        next_restart_at = NULL, updated_at = ?5
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, exit_code, exit_signal, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE operation_services SET phase = 'stopped', outcome = 'success'
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3",
                params![request_id, stack, service],
            )?,
            format!("operation service {request_id:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Records a child exit whose absence was directly observed by this daemon,
    /// without changing the running intent. A subsequent start is therefore safe.
    pub(crate) fn record_graceful_stop(
        &mut self,
        stack: &str,
        service: &str,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'stopped', effect_phase = 'shutdown_proven',
                        session_id = NULL, child_pid = NULL, child_starttime = NULL,
                        boot_id = NULL, last_exit_code = ?3, last_exit_signal = ?4,
                        next_restart_at = NULL, updated_at = ?5
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running'",
                params![stack, service, exit_code, exit_signal, now],
            )?,
            format!("running service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Cancels a durable automatic-restart delay during graceful daemon
    /// shutdown. The service is already proven absent, so no child or engine
    /// action is needed and the restart accounting remains intact.
    pub(crate) fn record_graceful_backoff_stop(
        &mut self,
        stack: &str,
        service: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'stopped', effect_phase = 'shutdown_proven',
                        next_restart_at = NULL, updated_at = ?3
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running'
                    AND observed_state = 'restarting' AND effect_phase = 'backoff'
                    AND next_restart_at IS NOT NULL
                    AND session_id IS NULL AND child_pid IS NULL
                    AND child_starttime IS NULL AND boot_id IS NULL",
                params![stack, service, now],
            )?,
            format!("proven backoff service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn resumable_services(&self, stack: &str) -> Result<Vec<String>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT name FROM services
              WHERE stack_name = ?1 AND active = 1 AND desired_state = 'running'
                AND observed_state = 'stopped' AND effect_phase = 'shutdown_proven'
              ORDER BY name",
        )?;
        statement
            .query_map([stack], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    pub(crate) fn finish_down(&mut self, request_id: &str, stack: &str) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let incomplete: i64 = transaction.query_row(
            "SELECT count(*) FROM operation_services
              WHERE request_id = ?1 AND outcome IS NOT 'success'",
            [request_id],
            |row| row.get(0),
        )?;
        if incomplete != 0 {
            return Err(Error::Conflict(format!(
                "cannot finish down operation {request_id:?}: {incomplete} service(s) are not stopped"
            )));
        }
        require_one(
            transaction.execute(
                "UPDATE stacks SET desired_state = 'stopped', observed_state = 'stopped',
                        updated_at = ?2 WHERE name = ?1",
                params![stack, now],
            )?,
            format!("stack {stack:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE operations SET phase = 'stopped', outcome = 'success', updated_at = ?2
                  WHERE request_id = ?1 AND stack_name = ?3 AND outcome IS NULL",
                params![request_id, now, stack],
            )?,
            format!("operation {request_id:?}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Terminalizes an online `down` failure without weakening ownership
    /// evidence. Services already proven stopped remain successful; every
    /// remaining exact-service row becomes unknown and retains any identity
    /// that may still be needed for diagnosis or containment.
    pub(crate) fn finalize_down_failure(
        &mut self,
        request_id: &str,
        stack: &str,
        code: &str,
        message: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let unfinished: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM operations
                  WHERE request_id = ?1 AND stack_name = ?2
                    AND operation = 'down' AND outcome IS NULL
             )",
            params![request_id, stack],
            |row| row.get(0),
        )?;
        if !unfinished {
            return Err(Error::NotFound(format!(
                "unfinished down operation {request_id:?} for stack {stack:?}"
            )));
        }

        transaction.execute(
            "UPDATE services
                SET desired_state = 'stopped', observed_state = 'unknown',
                    effect_phase = 'unknown', updated_at = ?3
              WHERE stack_name = ?2 AND active = 1
                AND EXISTS (
                    SELECT 1 FROM operation_services AS os
                     WHERE os.request_id = ?1 AND os.stack_name = ?2
                       AND os.service_name = services.name
                       AND os.outcome IS NOT 'success'
                )",
            params![request_id, stack, now],
        )?;
        transaction.execute(
            "UPDATE operation_services
                SET phase = 'unknown', outcome = 'failure',
                    error_code = ?3, error_message = ?4
              WHERE request_id = ?1 AND stack_name = ?2
                AND outcome IS NOT 'success'",
            params![request_id, stack, code, message],
        )?;
        require_one(
            transaction.execute(
                "UPDATE operations
                    SET phase = 'unknown', outcome = 'failure', error_code = ?3,
                        error_message = ?4, updated_at = ?5
                  WHERE request_id = ?1 AND stack_name = ?2
                    AND operation = 'down' AND outcome IS NULL",
                params![request_id, stack, code, message, now],
            )?,
            format!("unfinished down operation {request_id:?}"),
        )?;
        require_one(
            transaction.execute(
                "UPDATE stacks
                    SET desired_state = 'stopped', observed_state = 'unknown', updated_at = ?2
                  WHERE name = ?1",
                params![stack, now],
            )?,
            format!("stack {stack:?}"),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn service_identity(
        &self,
        stack: &str,
        service: &str,
    ) -> Result<ServiceIdentity, Error> {
        self.connection
            .query_row(
                "SELECT COALESCE(candidate.alias, v.current_alias),
                        COALESCE(candidate.generation, v.current_generation),
                        v.session_id, v.child_pid, v.child_starttime, v.boot_id
                   FROM services AS v
                   LEFT JOIN rootfs_generations AS candidate
                     ON candidate.stack_name = v.stack_name
                    AND candidate.service_name = v.name
                    AND candidate.role = 'candidate'
                    AND EXISTS (
                        SELECT 1
                          FROM operation_services AS os
                          JOIN operations AS o ON o.request_id = os.request_id
                         WHERE o.stack_name = v.stack_name
                           AND os.service_name = v.name
                           AND os.generation = candidate.generation
                           AND o.operation = 'up' AND o.outcome IS NULL
                    )
                  WHERE v.stack_name = ?1 AND v.name = ?2 AND v.active = 1",
                params![stack, service],
                |row| {
                    Ok(ServiceIdentity {
                        alias: row.get(0)?,
                        generation: row.get(1)?,
                        session_id: row.get(2)?,
                        child_pid: row.get(3)?,
                        child_starttime: row.get(4)?,
                        boot_id: row.get(5)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("service {stack:?}/{service:?}")))
    }

    pub(crate) fn existing_stack(&self, name: &str) -> Result<Option<ExistingStack>, Error> {
        let stack = self
            .connection
            .query_row(
                "SELECT manifest, manifest_base, committed_revision,
                        desired_state, observed_state
                   FROM stacks WHERE name = ?1",
                [name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((source, manifest_base, revision, desired_state, observed_state)) = stack else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT v.name, v.config_json, v.desired_state, v.observed_state,
                    v.effect_phase, v.current_alias, COALESCE(r.state, 'absent'),
                    v.restart_attempts,
                    v.restart_window_started_at, v.next_restart_at,
                    v.stdout_log_path, v.stderr_log_path
               FROM services AS v
               LEFT JOIN rootfs_generations AS r
                 ON r.stack_name = v.stack_name AND r.service_name = v.name
                AND r.generation = v.current_generation
              WHERE v.stack_name = ?1 AND v.active = 1 ORDER BY v.name",
        )?;
        let services = statement
            .query_map([name], |row| {
                Ok(ExistingService {
                    name: row.get(0)?,
                    config_json: row.get(1)?,
                    desired_state: row.get(2)?,
                    observed_state: row.get(3)?,
                    effect_phase: row.get(4)?,
                    alias: row.get(5)?,
                    rootfs_state: row.get(6)?,
                    restart_attempts: row.get(7)?,
                    restart_window_started_at: row.get(8)?,
                    next_restart_at: row.get(9)?,
                    stdout_log: PathBuf::from(row.get::<_, String>(10)?),
                    stderr_log: PathBuf::from(row.get::<_, String>(11)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(ExistingStack {
            source,
            manifest_base: PathBuf::from(manifest_base),
            revision,
            desired_state,
            observed_state,
            services,
        }))
    }

    /// Reconciles each service independently and never retries an engine effect.
    /// Any ambiguous service makes only its own stack unknown.
    pub(crate) fn reconcile_cold_start(&self) -> Result<usize, Error> {
        let now = unix_time()?;
        let transaction = self.connection.unchecked_transaction()?;
        let parents = unfinished_parent_operations(&transaction)?;
        let unfinished = unfinished_operation_rows(&transaction)?;
        let mut handled = BTreeSet::new();
        let mut changed = 0_usize;

        for row in &unfinished {
            let key = (row.stack_name.clone(), row.service_name.clone());
            handled.insert(key);
            let safe_before_effect = matches!(row.phase.as_str(), "intent" | "logs_prepared");
            let safe_after_install = row.phase == "installed";
            let current_start_safe =
                if matches!(row.phase.as_str(), "start_intent" | "shutdown_proven") {
                    match row.generation {
                        Some(generation) => transaction
                            .query_row(
                                "SELECT role = 'current' FROM rootfs_generations
                              WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                                params![row.stack_name, row.service_name, generation],
                                |result| result.get::<_, bool>(0),
                            )
                            .optional()?
                            .unwrap_or(false),
                        None => false,
                    }
                } else {
                    false
                };
            let (state, effect_phase, code, message) = if current_start_safe {
                (
                    "stopped",
                    "shutdown_proven",
                    "interrupted_before_engine",
                    "daemon stopped before invoking start on the committed rootfs",
                )
            } else if safe_before_effect {
                (
                    "failed",
                    "failed",
                    "interrupted_before_engine",
                    "daemon stopped before the engine invocation",
                )
            } else if safe_after_install {
                (
                    "failed",
                    "failed",
                    "interrupted_after_install",
                    "rootfs was installed but service start was not invoked",
                )
            } else {
                (
                    "unknown",
                    "unknown",
                    "cold_start_unknown",
                    "daemon lost ownership during an engine effect; no effect was retried",
                )
            };
            if let Some(generation) = row.generation {
                if safe_before_effect {
                    transaction.execute(
                        "UPDATE rootfs_generations
                            SET state = 'absent', role = 'retired', updated_at = ?4
                          WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                        params![row.stack_name, row.service_name, generation, now],
                    )?;
                } else if row.phase == "install_invoked" {
                    transaction.execute(
                        "UPDATE rootfs_generations SET state = 'unknown', updated_at = ?4
                          WHERE stack_name = ?1 AND service_name = ?2 AND generation = ?3",
                        params![row.stack_name, row.service_name, generation, now],
                    )?;
                }
            }
            changed += transaction.execute(
                "UPDATE services
                    SET observed_state = ?3, effect_phase = ?4,
                        session_id = NULL, child_pid = NULL, child_starttime = NULL,
                        boot_id = NULL, next_restart_at = NULL, updated_at = ?5
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND (observed_state != ?3 OR effect_phase != ?4
                         OR session_id IS NOT NULL OR child_pid IS NOT NULL)",
                params![row.stack_name, row.service_name, state, effect_phase, now],
            )?;
            transaction.execute(
                "UPDATE operation_services
                    SET phase = ?4, outcome = 'failure', error_code = ?5, error_message = ?6
                  WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3",
                params![
                    row.request_id,
                    row.stack_name,
                    row.service_name,
                    effect_phase,
                    code,
                    message
                ],
            )?;
        }

        let active = active_service_rows(&transaction)?;
        for row in active {
            if handled.contains(&(row.stack_name.clone(), row.service_name.clone())) {
                continue;
            }
            let proven_backoff = row.observed_state == "restarting"
                && row.effect_phase == "backoff"
                && row.next_restart_at.is_some()
                && row.session_id.is_none()
                && row.child_pid.is_none();
            if proven_backoff {
                continue;
            }
            changed += transaction.execute(
                "UPDATE services
                    SET observed_state = 'unknown', effect_phase = 'unknown',
                        session_id = NULL, child_pid = NULL, child_starttime = NULL,
                        boot_id = NULL, next_restart_at = NULL, updated_at = ?3
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![row.stack_name, row.service_name, now],
            )?;
        }

        for parent in parents {
            let (children, unsuccessful): (i64, i64) = transaction.query_row(
                "SELECT count(*),
                        COALESCE(sum(CASE WHEN outcome IS NOT 'success' THEN 1 ELSE 0 END), 0)
                   FROM operation_services WHERE request_id = ?1",
                [&parent.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let stack_services: i64 = transaction.query_row(
                "SELECT count(*) FROM services WHERE stack_name = ?1 AND active = 1",
                [&parent.stack_name],
                |row| row.get(0),
            )?;
            let unsafe_down_services: i64 = transaction.query_row(
                "SELECT count(*) FROM services
                  WHERE stack_name = ?1 AND active = 1
                    AND (desired_state != 'stopped' OR observed_state != 'stopped'
                         OR effect_phase != 'stopped'
                         OR session_id IS NOT NULL OR child_pid IS NOT NULL
                         OR child_starttime IS NOT NULL OR boot_id IS NOT NULL
                         OR next_restart_at IS NOT NULL)",
                [&parent.stack_name],
                |row| row.get(0),
            )?;
            let safe_down = parent.operation == "down"
                && children > 0
                && children == stack_services
                && unsuccessful == 0
                && unsafe_down_services == 0;
            if safe_down {
                require_one(
                    transaction.execute(
                        "UPDATE stacks
                            SET desired_state = 'stopped', observed_state = 'stopped',
                                updated_at = ?2 WHERE name = ?1",
                        params![parent.stack_name, now],
                    )?,
                    format!("stack {:?}", parent.stack_name),
                )?;
                require_one(
                    transaction.execute(
                        "UPDATE operations
                            SET phase = 'stopped', outcome = 'success', updated_at = ?2
                          WHERE request_id = ?1 AND outcome IS NULL",
                        params![parent.request_id, now],
                    )?,
                    format!("unfinished operation {:?}", parent.request_id),
                )?;
                continue;
            }

            let ambiguous_children: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM operation_services
                      WHERE request_id = ?1 AND phase = 'unknown'
                 )",
                [&parent.request_id],
                |row| row.get(0),
            )?;
            let ambiguous_services: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM services
                      WHERE stack_name = ?1 AND active = 1
                        AND (observed_state = 'unknown' OR effect_phase = 'unknown')
                 )",
                [&parent.stack_name],
                |row| row.get(0),
            )?;
            let effectful_parent_without_children = children == 0
                && matches!(
                    parent.phase.as_str(),
                    "install_invoked"
                        | "start_invoked"
                        | "starting"
                        | "running"
                        | "stop_invoked"
                        | "restart_stop_invoked"
                        | "restart_start_invoked"
                );
            let ambiguous =
                ambiguous_children || ambiguous_services || effectful_parent_without_children;
            require_one(
                transaction.execute(
                    "UPDATE operations
                        SET phase = ?2, outcome = 'failure', error_code = ?3,
                            error_message = ?4, updated_at = ?5
                      WHERE request_id = ?1 AND outcome IS NULL",
                    params![
                        parent.request_id,
                        if ambiguous { "unknown" } else { "failed" },
                        if ambiguous {
                            "cold_start_unknown"
                        } else {
                            "interrupted"
                        },
                        "daemon stopped before the operation committed; no effect was retried",
                        now
                    ],
                )?,
                format!("unfinished operation {:?}", parent.request_id),
            )?;
        }

        let stacks = service_stack_names(&transaction)?;
        for stack in stacks {
            refresh_stack_state(&transaction, &stack, now)?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub(crate) fn record_exit(
        &mut self,
        stack: &str,
        service: &str,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        let desired: String = transaction
            .query_row(
                "SELECT desired_state FROM services
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("service {stack:?}/{service:?}")))?;
        let state = if desired == "stopped" {
            "stopped"
        } else {
            "failed"
        };
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = ?3, effect_phase = 'exited', session_id = NULL,
                        child_pid = NULL, child_starttime = NULL, boot_id = NULL,
                        last_exit_code = ?4, last_exit_signal = ?5,
                        next_restart_at = NULL, updated_at = ?6
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, state, exit_code, exit_signal, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Records supervision ambiguity without discarding the last persisted
    /// identity. The runtime may still need that identity for diagnostics or
    /// containment, but must not claim the service is absent.
    pub(crate) fn mark_runtime_unknown(&mut self, stack: &str, service: &str) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'unknown', effect_phase = 'unknown',
                        next_restart_at = NULL, updated_at = ?3
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn schedule_restart(
        &mut self,
        stack: &str,
        service: &str,
        attempts: u32,
        window_started_at: i64,
        next_attempt_at: i64,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'restarting', effect_phase = 'backoff',
                        restart_attempts = ?3, restart_window_started_at = ?4,
                        next_restart_at = ?5, updated_at = ?6
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running'
                    AND session_id IS NULL AND child_pid IS NULL",
                params![
                    stack,
                    service,
                    i64::from(attempts),
                    window_started_at,
                    next_attempt_at,
                    now
                ],
            )?,
            format!("running service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Defers a proven-absent scheduled restart without consuming another
    /// attempt. The deadline can only move forward while the service remains
    /// in the exact durable backoff state.
    pub(crate) fn defer_restart(
        &mut self,
        stack: &str,
        service: &str,
        next_attempt_at: i64,
    ) -> Result<(), Error> {
        require_one(
            self.connection.execute(
                "UPDATE services
                    SET next_restart_at = ?3, updated_at = ?4
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running'
                    AND observed_state = 'restarting' AND effect_phase = 'backoff'
                    AND next_restart_at IS NOT NULL AND next_restart_at < ?3
                    AND session_id IS NULL AND child_pid IS NULL
                    AND child_starttime IS NULL AND boot_id IS NULL",
                params![stack, service, next_attempt_at, unix_time()?],
            )?,
            format!("scheduled restart {stack:?}/{service:?}"),
        )
    }

    /// Restores the same backoff attempt when restart intent was persisted but
    /// a pre-spawn failure proves that no child identity was acquired.
    pub(crate) fn restore_restart_backoff(
        &mut self,
        stack: &str,
        service: &str,
        next_attempt_at: i64,
    ) -> Result<(), Error> {
        require_one(
            self.connection.execute(
                "UPDATE services
                    SET observed_state = 'restarting', effect_phase = 'backoff',
                        next_restart_at = ?3, updated_at = ?4
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running' AND observed_state = 'starting'
                    AND effect_phase = 'restart_invoked'
                    AND session_id IS NULL AND child_pid IS NULL
                    AND child_starttime IS NULL AND boot_id IS NULL",
                params![stack, service, next_attempt_at, unix_time()?],
            )?,
            format!("invoked restart without child {stack:?}/{service:?}"),
        )
    }

    pub(crate) fn due_restarts(&self, now: i64) -> Result<Vec<ScheduledRestart>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT stack_name, name, next_restart_at, restart_attempts
               FROM services
              WHERE active = 1 AND desired_state = 'running' AND observed_state = 'restarting'
                AND effect_phase = 'backoff' AND next_restart_at <= ?1
                AND session_id IS NULL AND child_pid IS NULL
              ORDER BY next_restart_at, stack_name, name",
        )?;
        statement
            .query_map([now], |row| {
                Ok(ScheduledRestart {
                    stack_name: row.get(0)?,
                    service_name: row.get(1)?,
                    at: row.get(2)?,
                    attempts: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    /// Must be called before invoking the engine for an automatic restart.
    pub(crate) fn mark_restart_invoked(&mut self, stack: &str, service: &str) -> Result<(), Error> {
        let now = unix_time()?;
        require_one(
            self.connection.execute(
                "UPDATE services
                    SET observed_state = 'starting', effect_phase = 'restart_invoked',
                        next_restart_at = NULL, updated_at = ?3
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1
                    AND desired_state = 'running' AND effect_phase = 'backoff'
                    AND next_restart_at IS NOT NULL
                    AND session_id IS NULL AND child_pid IS NULL",
                params![stack, service, now],
            )?,
            format!("scheduled restart {stack:?}/{service:?}"),
        )
    }

    pub(crate) fn mark_restart_starting(
        &mut self,
        stack: &str,
        service: &str,
        child_pid: u32,
        child_starttime: u64,
        boot_id: &str,
    ) -> Result<(), Error> {
        let child_starttime = i64::try_from(child_starttime)
            .map_err(|_| Error::Io(io::Error::other("process starttime exceeds SQLite INTEGER")))?;
        require_one(
            self.connection.execute(
                "UPDATE services
                    SET observed_state = 'starting', effect_phase = 'restart_starting',
                        child_pid = ?3, child_starttime = ?4, boot_id = ?5,
                        updated_at = ?6
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![
                    stack,
                    service,
                    child_pid,
                    child_starttime,
                    boot_id,
                    unix_time()?
                ],
            )?,
            format!("service {stack:?}/{service:?}"),
        )
    }

    pub(crate) fn mark_restart_running(
        &mut self,
        stack: &str,
        service: &str,
        session_id: u32,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        require_one(
            transaction.execute(
                "UPDATE services
                    SET observed_state = 'running', effect_phase = 'committed',
                        session_id = ?3, next_restart_at = NULL, updated_at = ?4
                  WHERE stack_name = ?1 AND name = ?2 AND active = 1",
                params![stack, service, session_id, now],
            )?,
            format!("service {stack:?}/{service:?}"),
        )?;
        refresh_stack_state(&transaction, stack, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn operation_replay(
        &self,
        request_id: &str,
    ) -> Result<Option<OperationReplay>, Error> {
        self.connection
            .query_row(
                "SELECT operation, stack_name, candidate_manifest, manifest_base,
                        target_service, response_json
                   FROM operations WHERE request_id = ?1",
                [request_id],
                |row| {
                    Ok(OperationReplay {
                        operation: row.get(0)?,
                        stack_name: row.get(1)?,
                        candidate_manifest: row.get(2)?,
                        manifest_base: row.get(3)?,
                        target_service: row.get(4)?,
                        response_json: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub(crate) fn record_noop_up(
        &self,
        request_id: &str,
        stack: &str,
        candidate_manifest: &str,
        manifest_base: &Path,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let revision: i64 = self.connection.query_row(
            "SELECT committed_revision FROM stacks WHERE name = ?1",
            [stack],
            |row| row.get(0),
        )?;
        self.connection.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 outcome, created_at, updated_at
             ) VALUES (?1, ?2, 'up', 'already_running', ?3, ?4, ?5, NULL,
                       'success', ?6, ?6)",
            params![
                request_id,
                stack,
                candidate_manifest,
                manifest_base.to_string_lossy(),
                revision,
                now
            ],
        )?;
        Ok(())
    }

    pub(crate) fn cache_response(
        &self,
        request_id: &str,
        response: &crate::protocol::Response,
    ) -> Result<(), Error> {
        let json = serde_json::to_string(response).expect("Response is serializable");
        self.connection.execute(
            "UPDATE operations SET response_json = ?2, updated_at = ?3 WHERE request_id = ?1",
            params![request_id, json, unix_time()?],
        )?;
        Ok(())
    }

    fn set_candidate_phase(
        &mut self,
        request_id: &str,
        stack: &str,
        service: &str,
        phase: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        set_service_phase(&transaction, request_id, stack, service, phase, now)?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Debug)]
struct OperationRow {
    request_id: String,
    stack_name: String,
    service_name: String,
    phase: String,
    candidate_config_json: Option<String>,
    generation: Option<i64>,
}

#[derive(Debug)]
struct UnfinishedParentOperation {
    request_id: String,
    stack_name: String,
    operation: String,
    phase: String,
}

#[derive(Debug)]
struct CandidateGeneration {
    generation: i64,
    alias: String,
    image: String,
    state: String,
    role: String,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
}

fn recovery_generation(
    transaction: &Transaction<'_>,
    stack: &str,
    service: &str,
    generation: i64,
) -> Result<CandidateGeneration, Error> {
    transaction
        .query_row(
            "SELECT r.generation, r.alias, r.image, r.state, r.role,
                    v.stdout_log_path, v.stderr_log_path
           FROM rootfs_generations AS r
           JOIN services AS v
             ON v.stack_name = r.stack_name AND v.name = r.service_name
          WHERE r.stack_name = ?1 AND r.service_name = ?2 AND r.generation = ?3",
            params![stack, service, generation],
            |row| {
                Ok(CandidateGeneration {
                    generation: row.get(0)?,
                    alias: row.get(1)?,
                    image: row.get(2)?,
                    state: row.get(3)?,
                    role: row.get(4)?,
                    stdout_log: PathBuf::from(row.get::<_, String>(5)?),
                    stderr_log: PathBuf::from(row.get::<_, String>(6)?),
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            Error::Conflict(format!(
                "candidate recovery generation {stack:?}/{service:?}/{generation} no longer exists"
            ))
        })
}

/// Returns true only when the newest failed candidate can be retired without
/// losing ambiguity evidence. This is deliberately independent of the new
/// payload: a corrected manifest should fall back to a fresh `begin_up`, while
/// an invoked/unknown or structurally incomplete candidate must still block it.
fn terminal_candidate_allows_fresh_up(
    transaction: &Transaction<'_>,
    stack: &str,
    expected_services: &BTreeSet<String>,
    request_id: &str,
) -> Result<bool, Error> {
    let rows = operation_rows(transaction, request_id)?;
    let names = rows
        .iter()
        .map(|row| row.service_name.clone())
        .collect::<BTreeSet<_>>();
    if names != *expected_services
        || rows.len() != expected_services.len()
        || rows.iter().any(|row| row.stack_name != stack)
    {
        return Ok(false);
    }

    for row in rows {
        let Some(generation) = row.generation else {
            return Ok(false);
        };
        let latest: Option<i64> = transaction.query_row(
            "SELECT max(generation) FROM rootfs_generations
              WHERE stack_name = ?1 AND service_name = ?2",
            params![stack, row.service_name],
            |result| result.get(0),
        )?;
        if latest != Some(generation) {
            return Ok(false);
        }
        let candidate = recovery_generation(transaction, stack, &row.service_name, generation)?;
        let safe = (candidate.state == "installed"
            && matches!(candidate.role.as_str(), "candidate" | "retired"))
            || (candidate.state == "absent"
                && matches!(candidate.role.as_str(), "candidate" | "retired"))
            || (candidate.state == "preparing"
                && candidate.role == "candidate"
                && row.phase == "failed_pre_effect");
        if !safe {
            return Ok(false);
        }
    }
    Ok(true)
}

fn revision_zero_candidate_allows_fresh_up(
    transaction: &Transaction<'_>,
    stack: &str,
    active_services: &BTreeSet<String>,
) -> Result<bool, Error> {
    if active_services.is_empty() {
        return Ok(true);
    }
    let mut statement = transaction.prepare(
        "SELECT request_id FROM operations
          WHERE stack_name = ?1 AND operation = 'up' AND outcome = 'failure'
            AND candidate_revision = 1
          ORDER BY request_id",
    )?;
    let request_ids = statement
        .query_map([stack], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for request_id in request_ids {
        if terminal_candidate_allows_fresh_up(transaction, stack, active_services, &request_id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn set_service_phase(
    transaction: &Transaction<'_>,
    request_id: &str,
    stack: &str,
    service: &str,
    phase: &str,
    now: i64,
) -> Result<(), Error> {
    require_one(
        transaction.execute(
            "UPDATE operation_services SET phase = ?4
              WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3
                AND outcome IS NULL",
            params![request_id, stack, service, phase],
        )?,
        format!("unfinished operation service {request_id:?}/{service:?}"),
    )?;
    require_one(
        transaction.execute(
            "UPDATE services SET effect_phase = ?3, updated_at = ?4
              WHERE stack_name = ?1 AND name = ?2 AND active = 1",
            params![stack, service, phase, now],
        )?,
        format!("service {stack:?}/{service:?}"),
    )?;
    Ok(())
}

fn operation_generation(
    transaction: &Transaction<'_>,
    request_id: &str,
    stack: &str,
    service: &str,
) -> Result<i64, Error> {
    transaction
        .query_row(
            "SELECT generation FROM operation_services
              WHERE request_id = ?1 AND stack_name = ?2 AND service_name = ?3",
            params![request_id, stack, service],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("operation service {request_id:?}/{service:?}")))
}

fn operation_rows(
    transaction: &Transaction<'_>,
    request_id: &str,
) -> Result<Vec<OperationRow>, Error> {
    let mut statement = transaction.prepare(
        "SELECT request_id, stack_name, service_name, phase,
                candidate_config_json, generation
           FROM operation_services WHERE request_id = ?1 ORDER BY ordinal",
    )?;
    statement
        .query_map([request_id], |row| {
            Ok(OperationRow {
                request_id: row.get(0)?,
                stack_name: row.get(1)?,
                service_name: row.get(2)?,
                phase: row.get(3)?,
                candidate_config_json: row.get(4)?,
                generation: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn unfinished_operation_rows(transaction: &Transaction<'_>) -> Result<Vec<OperationRow>, Error> {
    let mut statement = transaction.prepare(
        "SELECT os.request_id, os.stack_name, os.service_name, os.phase,
                os.candidate_config_json, os.generation
           FROM operation_services AS os
           JOIN operations AS o ON o.request_id = os.request_id
          WHERE o.outcome IS NULL AND os.outcome IS NULL
          ORDER BY os.stack_name, os.ordinal",
    )?;
    statement
        .query_map([], |row| {
            Ok(OperationRow {
                request_id: row.get(0)?,
                stack_name: row.get(1)?,
                service_name: row.get(2)?,
                phase: row.get(3)?,
                candidate_config_json: row.get(4)?,
                generation: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn unfinished_parent_operations(
    transaction: &Transaction<'_>,
) -> Result<Vec<UnfinishedParentOperation>, Error> {
    let mut statement = transaction.prepare(
        "SELECT request_id, stack_name, operation, phase
           FROM operations WHERE outcome IS NULL
          ORDER BY stack_name, created_at, request_id",
    )?;
    statement
        .query_map([], |row| {
            Ok(UnfinishedParentOperation {
                request_id: row.get(0)?,
                stack_name: row.get(1)?,
                operation: row.get(2)?,
                phase: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

#[derive(Debug)]
struct ActiveServiceRow {
    stack_name: String,
    service_name: String,
    observed_state: String,
    effect_phase: String,
    next_restart_at: Option<i64>,
    session_id: Option<i64>,
    child_pid: Option<i64>,
}

fn active_service_rows(transaction: &Transaction<'_>) -> Result<Vec<ActiveServiceRow>, Error> {
    let mut statement = transaction.prepare(
        "SELECT stack_name, name, observed_state, effect_phase,
                next_restart_at, session_id, child_pid
           FROM services
          WHERE active = 1
            AND (observed_state IN ('preparing', 'starting', 'running', 'stopping', 'restarting')
                 OR session_id IS NOT NULL OR child_pid IS NOT NULL)
          ORDER BY stack_name, name",
    )?;
    statement
        .query_map([], |row| {
            Ok(ActiveServiceRow {
                stack_name: row.get(0)?,
                service_name: row.get(1)?,
                observed_state: row.get(2)?,
                effect_phase: row.get(3)?,
                next_restart_at: row.get(4)?,
                session_id: row.get(5)?,
                child_pid: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn refresh_stack_state(transaction: &Transaction<'_>, stack: &str, now: i64) -> Result<(), Error> {
    let desired: String = transaction
        .query_row(
            "SELECT desired_state FROM stacks WHERE name = ?1",
            [stack],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("stack {stack:?}")))?;
    let mut statement = transaction.prepare(
        "SELECT observed_state FROM services
              WHERE stack_name = ?1 AND active = 1 ORDER BY name",
    )?;
    let states = statement
        .query_map([stack], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let state = if states.iter().any(|value| value == "unknown") {
        "unknown"
    } else if desired == "stopped" {
        if states.iter().all(|value| value == "stopped") {
            "stopped"
        } else {
            "stopping"
        }
    } else if states.iter().all(|value| value == "running") {
        "running"
    } else if states.iter().any(|value| value == "failed") {
        "failed"
    } else if states
        .iter()
        .any(|value| matches!(value.as_str(), "stopping" | "restarting"))
    {
        "restarting"
    } else {
        "starting"
    };
    require_one(
        transaction.execute(
            "UPDATE stacks SET observed_state = ?2, updated_at = ?3 WHERE name = ?1",
            params![stack, state, now],
        )?,
        format!("stack {stack:?}"),
    )
}

fn reject_unfinished_operation(transaction: &Transaction<'_>, stack: &str) -> Result<(), Error> {
    let request: Option<String> = transaction
        .query_row(
            "SELECT request_id FROM operations
              WHERE stack_name = ?1 AND outcome IS NULL LIMIT 1",
            [stack],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(request) = request {
        Err(Error::Conflict(format!(
            "stack {stack:?} has unfinished operation {request:?}"
        )))
    } else {
        Ok(())
    }
}

fn service_names(transaction: &Transaction<'_>, stack: &str) -> Result<BTreeSet<String>, Error> {
    let mut statement = transaction
        .prepare("SELECT name FROM services WHERE stack_name = ?1 AND active = 1 ORDER BY name")?;
    statement
        .query_map([stack], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(Error::from)
}

fn service_stack_names(transaction: &Transaction<'_>) -> Result<Vec<String>, Error> {
    let mut statement = transaction
        .prepare("SELECT DISTINCT stack_name FROM services WHERE active = 1 ORDER BY stack_name")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn require_one(changed: usize, subject: String) -> Result<(), Error> {
    if changed == 1 {
        Ok(())
    } else {
        Err(Error::NotFound(subject))
    }
}

fn service_config_json(service: &crate::manifest::Service) -> String {
    let restart = match service.restart {
        crate::manifest::RestartPolicy::No => "no",
        crate::manifest::RestartPolicy::OnFailure => "on-failure",
        crate::manifest::RestartPolicy::Always => "always",
    };
    let mounts = service
        .mounts
        .iter()
        .map(|mount| {
            serde_json::json!({
                "kind": match mount.kind {
                    crate::manifest::MountKind::Volume => "volume",
                    crate::manifest::MountKind::Bind => "bind",
                },
                "source": mount.source,
                "target": mount.target,
            })
        })
        .collect::<Vec<_>>();
    let ports = service
        .ports
        .iter()
        .map(|port| serde_json::json!({"address": port.address, "port": port.port}))
        .collect::<Vec<_>>();
    serde_json::json!({
        "image": service.image,
        "command": service.command,
        "environment": service.environment,
        "mounts": mounts,
        "ports": ports,
        "dependsOn": service.depends_on,
        "restart": restart,
    })
    .to_string()
}

#[cfg(debug_assertions)]
fn apply_debug_storage_limit(connection: &Connection) -> Result<(), Error> {
    let Some(raw_limit) = std::env::var_os("TERMUX_STACKS_SQLITE_MAX_PAGES") else {
        return Ok(());
    };
    let raw_limit = raw_limit.into_string().map_err(|_| {
        Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TERMUX_STACKS_SQLITE_MAX_PAGES must be UTF-8",
        ))
    })?;
    let limit = raw_limit.parse::<u32>().map_err(|_| {
        Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TERMUX_STACKS_SQLITE_MAX_PAGES must be a positive decimal integer",
        ))
    })?;
    if limit == 0 {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TERMUX_STACKS_SQLITE_MAX_PAGES must be a positive decimal integer",
        )));
    }
    let actual: u32 =
        connection.query_row(&format!("PRAGMA max_page_count = {limit}"), [], |row| {
            row.get(0)
        })?;
    if actual != limit {
        return Err(Error::Io(io::Error::other(format!(
            "SQLite refused debug max_page_count {limit}; active value is {actual}"
        ))));
    }
    Ok(())
}

#[cfg(not(debug_assertions))]
fn apply_debug_storage_limit(_connection: &Connection) -> Result<(), Error> {
    Ok(())
}

fn prepare_database_file(path: &Path) -> Result<(), Error> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_file()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(Error::UnsafePath(path.to_path_buf()));
        }
        return Ok(());
    }

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<(), Error> {
    let installation_id = random_identifier()?;
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE meta (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         ) STRICT;",
    )?;
    create_v3_tables(&transaction)?;
    transaction.execute(
        "INSERT INTO meta(key, value) VALUES ('installation_id', ?1)",
        [installation_id],
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn create_v3_tables(transaction: &Transaction<'_>) -> Result<(), Error> {
    transaction.execute_batch(
        "CREATE TABLE stacks (
             name TEXT PRIMARY KEY,
             desired_state TEXT NOT NULL,
             observed_state TEXT NOT NULL,
             manifest TEXT NOT NULL,
             manifest_base TEXT NOT NULL,
             committed_revision INTEGER NOT NULL CHECK (committed_revision >= 0),
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE services (
             stack_name TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
             name TEXT NOT NULL,
             active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
             config_json TEXT NOT NULL,
             desired_state TEXT NOT NULL,
             observed_state TEXT NOT NULL,
             current_alias TEXT,
             current_generation INTEGER,
             effect_phase TEXT NOT NULL,
             session_id INTEGER,
             child_pid INTEGER,
             child_starttime INTEGER,
             boot_id TEXT,
             last_exit_code INTEGER,
             last_exit_signal INTEGER,
             restart_attempts INTEGER NOT NULL DEFAULT 0 CHECK (restart_attempts >= 0),
             restart_window_started_at INTEGER,
             next_restart_at INTEGER,
             stdout_log_path TEXT NOT NULL,
             stderr_log_path TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             PRIMARY KEY (stack_name, name),
             CHECK ((current_alias IS NULL) = (current_generation IS NULL))
         ) STRICT;
         CREATE TABLE rootfs_generations (
             stack_name TEXT NOT NULL,
             service_name TEXT NOT NULL,
             generation INTEGER NOT NULL CHECK (generation > 0),
             alias TEXT NOT NULL UNIQUE,
             image TEXT NOT NULL,
             state TEXT NOT NULL,
             role TEXT NOT NULL CHECK (role IN ('candidate', 'current', 'retired')),
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             PRIMARY KEY (stack_name, service_name, generation),
             FOREIGN KEY (stack_name, service_name)
                 REFERENCES services(stack_name, name) ON DELETE CASCADE
         ) STRICT;
         CREATE UNIQUE INDEX one_current_rootfs_per_service
             ON rootfs_generations(stack_name, service_name) WHERE role = 'current';
         CREATE UNIQUE INDEX one_candidate_rootfs_per_service
             ON rootfs_generations(stack_name, service_name) WHERE role = 'candidate';
         CREATE TRIGGER rootfs_generation_identity_immutable
             BEFORE UPDATE OF alias, image ON rootfs_generations
             BEGIN SELECT RAISE(ABORT, 'rootfs generation identity is immutable'); END;
         CREATE TABLE operations (
             request_id TEXT PRIMARY KEY,
             stack_name TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
             operation TEXT NOT NULL,
             phase TEXT NOT NULL,
             candidate_manifest TEXT,
             manifest_base TEXT,
             candidate_revision INTEGER,
             target_service TEXT,
             outcome TEXT,
             error_code TEXT,
             error_message TEXT,
             response_json TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         ) STRICT;
         CREATE UNIQUE INDEX one_unfinished_operation_per_stack
             ON operations(stack_name) WHERE outcome IS NULL;
         CREATE TABLE operation_services (
             request_id TEXT NOT NULL REFERENCES operations(request_id) ON DELETE CASCADE,
             stack_name TEXT NOT NULL,
             service_name TEXT NOT NULL,
             ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
             phase TEXT NOT NULL,
             outcome TEXT,
             candidate_config_json TEXT,
             generation INTEGER,
             error_code TEXT,
             error_message TEXT,
             PRIMARY KEY (request_id, service_name),
             UNIQUE (request_id, ordinal)
         ) STRICT;",
    )?;
    Ok(())
}

#[derive(Debug)]
struct V2Stack {
    name: String,
    desired_state: String,
    observed_state: String,
    manifest: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug)]
struct V2Service {
    stack_name: String,
    name: String,
    image: String,
    command_json: String,
    alias: String,
    observed_state: String,
    rootfs_state: String,
    last_exit_code: Option<i64>,
    stdout_log_path: String,
    stderr_log_path: String,
}

#[derive(Debug)]
struct V2Operation {
    request_id: String,
    stack_name: String,
    operation: String,
    phase: String,
    outcome: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    response_json: Option<String>,
    created_at: i64,
    updated_at: i64,
}

/// The migration is one SQLite transaction and deliberately performs no engine effect.
fn migrate_v2_to_v3(connection: &Connection) -> Result<(), Error> {
    let transaction = connection.unchecked_transaction()?;
    let stacks = load_v2_stacks(&transaction)?;
    let services = load_v2_services(&transaction)?;
    let operations = load_v2_operations(&transaction)?;
    let successful_up = operations
        .iter()
        .filter(|operation| {
            operation.operation == "up" && operation.outcome.as_deref() == Some("success")
        })
        .map(|operation| operation.stack_name.clone())
        .collect::<BTreeSet<_>>();
    let incomplete = operations
        .iter()
        .filter(|operation| operation.outcome.is_none())
        .map(|operation| operation.stack_name.clone())
        .collect::<BTreeSet<_>>();

    transaction.execute_batch(
        "ALTER TABLE stacks RENAME TO stacks_v2;
         ALTER TABLE services RENAME TO services_v2;
         ALTER TABLE operations RENAME TO operations_v2;",
    )?;
    create_v3_tables(&transaction)?;

    for stack in &stacks {
        let ambiguous_service = services.iter().any(|service| {
            service.stack_name == stack.name
                && (matches!(
                    service.observed_state.as_str(),
                    "preparing" | "starting" | "running" | "stopping"
                ) || matches!(service.rootfs_state.as_str(), "preparing" | "unknown"))
        });
        let ambiguous = incomplete.contains(&stack.name) || ambiguous_service;
        let committed = successful_up.contains(&stack.name);
        transaction.execute(
            "INSERT INTO stacks(
                 name, desired_state, observed_state, manifest, manifest_base,
                 committed_revision, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7)",
            params![
                stack.name,
                stack.desired_state,
                if ambiguous {
                    "unknown"
                } else {
                    stack.observed_state.as_str()
                },
                if committed {
                    stack.manifest.as_str()
                } else {
                    ""
                },
                if committed { 1_i64 } else { 0_i64 },
                stack.created_at,
                stack.updated_at
            ],
        )?;
    }

    for service in &services {
        let ambiguous = incomplete.contains(&service.stack_name)
            || matches!(
                service.observed_state.as_str(),
                "preparing" | "starting" | "running" | "stopping"
            )
            || matches!(service.rootfs_state.as_str(), "preparing" | "unknown");
        let known_current = successful_up.contains(&service.stack_name)
            && !ambiguous
            && service.rootfs_state == "installed"
            && matches!(service.observed_state.as_str(), "stopped" | "failed");
        let observed = if ambiguous {
            "unknown"
        } else {
            service.observed_state.as_str()
        };
        let command = serde_json::from_str::<serde_json::Value>(&service.command_json)
            .unwrap_or(serde_json::Value::Null);
        let config = serde_json::json!({
            "image": service.image,
            "command": command,
            "environment": {},
            "mounts": [],
            "ports": [],
            "dependsOn": [],
            "restart": "no",
        })
        .to_string();
        transaction.execute(
            "INSERT INTO services(
                 stack_name, name, active, config_json, desired_state, observed_state,
                 current_alias, current_generation, effect_phase,
                 session_id, child_pid, child_starttime, boot_id,
                 last_exit_code, last_exit_signal, restart_attempts,
                 restart_window_started_at, next_restart_at,
                 stdout_log_path, stderr_log_path, created_at, updated_at
             ) VALUES (
                 ?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8,
                 NULL, NULL, NULL, NULL, ?9, NULL, 0, NULL, NULL,
                 ?10, ?11, ?12, ?12
             )",
            params![
                service.stack_name,
                service.name,
                config,
                if service.observed_state == "stopped" {
                    "stopped"
                } else {
                    "running"
                },
                observed,
                if known_current {
                    Some(service.alias.as_str())
                } else {
                    None
                },
                if known_current { Some(1_i64) } else { None },
                if ambiguous {
                    "unknown"
                } else if known_current {
                    "committed"
                } else {
                    observed
                },
                service.last_exit_code,
                service.stdout_log_path,
                service.stderr_log_path,
                unix_time()?
            ],
        )?;
        if !service.alias.is_empty() {
            transaction.execute(
                "INSERT INTO rootfs_generations(
                     stack_name, service_name, generation, alias, image, state, role,
                     created_at, updated_at
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    service.stack_name,
                    service.name,
                    service.alias,
                    service.image,
                    if ambiguous {
                        "unknown"
                    } else {
                        service.rootfs_state.as_str()
                    },
                    if known_current { "current" } else { "retired" },
                    unix_time()?
                ],
            )?;
        }
    }

    for operation in &operations {
        let migrated_incomplete = operation.outcome.is_none();
        let candidate_revision = if operation.operation == "up" {
            Some(1_i64)
        } else {
            None
        };
        let candidate_manifest = if operation.operation == "up" {
            stacks
                .iter()
                .find(|stack| stack.name == operation.stack_name)
                .map(|stack| stack.manifest.as_str())
        } else {
            None
        };
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, candidate_manifest,
                 manifest_base, candidate_revision, target_service,
                 outcome, error_code, error_message, response_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                operation.request_id,
                operation.stack_name,
                operation.operation,
                if migrated_incomplete {
                    "unknown"
                } else {
                    operation.phase.as_str()
                },
                candidate_manifest,
                candidate_manifest.map(|_| ""),
                candidate_revision,
                if migrated_incomplete {
                    Some("failure")
                } else {
                    operation.outcome.as_deref()
                },
                if migrated_incomplete {
                    Some("migration_unknown")
                } else {
                    operation.error_code.as_deref()
                },
                if migrated_incomplete {
                    Some("v2 operation was incomplete at migration; no engine effect was retried")
                } else {
                    operation.error_message.as_deref()
                },
                operation.response_json,
                operation.created_at,
                operation.updated_at
            ],
        )?;
        for (ordinal, service) in services
            .iter()
            .filter(|service| service.stack_name == operation.stack_name)
            .enumerate()
        {
            let ordinal = i64::try_from(ordinal).expect("v2 service count fits SQLite INTEGER");
            let config: String = transaction.query_row(
                "SELECT config_json FROM services WHERE stack_name = ?1 AND name = ?2",
                params![service.stack_name, service.name],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO operation_services(
                     request_id, stack_name, service_name, ordinal, phase, outcome,
                     candidate_config_json, generation, error_code, error_message
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    operation.request_id,
                    operation.stack_name,
                    service.name,
                    ordinal,
                    if migrated_incomplete { "unknown" } else { operation.phase.as_str() },
                    if migrated_incomplete { Some("failure") } else { operation.outcome.as_deref() },
                    if operation.operation == "up" { Some(config) } else { None },
                    if operation.operation == "up" && !service.alias.is_empty() { Some(1_i64) } else { None },
                    if migrated_incomplete { Some("migration_unknown") } else { operation.error_code.as_deref() },
                    if migrated_incomplete {
                        Some("v2 operation was incomplete at migration; no engine effect was retried")
                    } else {
                        operation.error_message.as_deref()
                    }
                ],
            )?;
        }
    }

    transaction.execute_batch(
        "DROP TABLE operations_v2;
         DROP TABLE services_v2;
         DROP TABLE stacks_v2;",
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn load_v2_stacks(transaction: &Transaction<'_>) -> Result<Vec<V2Stack>, Error> {
    let mut statement = transaction.prepare(
        "SELECT name, desired_state, observed_state, manifest, created_at, updated_at
           FROM stacks ORDER BY name",
    )?;
    statement
        .query_map([], |row| {
            Ok(V2Stack {
                name: row.get(0)?,
                desired_state: row.get(1)?,
                observed_state: row.get(2)?,
                manifest: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn load_v2_services(transaction: &Transaction<'_>) -> Result<Vec<V2Service>, Error> {
    let mut statement = transaction.prepare(
        "SELECT stack_name, name, image, command_json, alias, observed_state,
                rootfs_state, last_exit_code, stdout_log_path, stderr_log_path
           FROM services ORDER BY stack_name, name",
    )?;
    statement
        .query_map([], |row| {
            Ok(V2Service {
                stack_name: row.get(0)?,
                name: row.get(1)?,
                image: row.get(2)?,
                command_json: row.get(3)?,
                alias: row.get(4)?,
                observed_state: row.get(5)?,
                rootfs_state: row.get(6)?,
                last_exit_code: row.get(7)?,
                stdout_log_path: row.get(8)?,
                stderr_log_path: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn load_v2_operations(transaction: &Transaction<'_>) -> Result<Vec<V2Operation>, Error> {
    let mut statement = transaction.prepare(
        "SELECT request_id, stack_name, operation, phase, outcome, error_code,
                error_message, response_json, created_at, updated_at
           FROM operations ORDER BY created_at, request_id",
    )?;
    statement
        .query_map([], |row| {
            Ok(V2Operation {
                request_id: row.get(0)?,
                stack_name: row.get(1)?,
                operation: row.get(2)?,
                phase: row.get(3)?,
                outcome: row.get(4)?,
                error_code: row.get(5)?,
                error_message: row.get(6)?,
                response_json: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn random_identifier() -> Result<String, Error> {
    let mut bytes = [0_u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(output)
}

fn unix_time() -> Result<i64, Error> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| io::Error::other(format!("system clock before Unix epoch: {error}")))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| {
        Error::Io(io::Error::other(
            "system time does not fit in SQLite INTEGER",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{Error, ServicePlan, Store};
    use crate::manifest::{Manifest, RestartPolicy, Service};
    use crate::paths::RuntimePaths;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn service(name: &str, image: &str) -> Service {
        Service {
            name: name.into(),
            image: image.into(),
            command: Some(vec!["sleep".into(), "60".into()]),
            environment: BTreeMap::new(),
            mounts: Vec::new(),
            ports: Vec::new(),
            depends_on: Vec::new(),
            restart: RestartPolicy::No,
        }
    }

    fn manifest(name: &str, names: &[&str]) -> Manifest {
        let services = names
            .iter()
            .map(|name| (name.to_string(), service(name, "alpine:3.22")))
            .collect();
        Manifest {
            name: name.into(),
            services,
            volumes: BTreeSet::new(),
        }
    }

    fn plans(prefix: &Path, names: &[&str]) -> BTreeMap<String, ServicePlan> {
        names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    ServicePlan {
                        alias: format!(
                            "txs-test-{name}-{}",
                            prefix.file_name().unwrap().to_string_lossy()
                        ),
                        stdout_log: prefix.join(format!("{name}.stdout.log")),
                        stderr_log: prefix.join(format!("{name}.stderr.log")),
                    },
                )
            })
            .collect()
    }

    fn drive_up(store: &mut Store, request: &str, stack: &str, names: &[&str]) {
        for name in names {
            store
                .mark_logs_prepared(request, stack, name)
                .expect("logs");
            store
                .mark_install_invoked(request, stack, name)
                .expect("install intent");
            store
                .mark_installed(request, stack, name)
                .expect("installed");
            store
                .mark_start_invoked(request, stack, name)
                .expect("start intent");
            store
                .mark_starting(request, stack, name, 100, 200, "boot")
                .expect("starting");
            store
                .mark_running(request, stack, name, 100)
                .expect("running");
        }
        store.commit_up(request, stack).expect("commit up");
    }

    fn failed_candidate_fixture(
        label: &str,
        installed: bool,
    ) -> (PathBuf, Store, Manifest, String, PathBuf, String) {
        let prefix = crate::paths::test_prefix(label);
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let initial = manifest("demo", &["app"]);
        store
            .begin_up(
                "up-1",
                "initial-source",
                Path::new("/initial"),
                &initial,
                &plans(&prefix.join("initial"), &["app"]),
            )
            .expect("initial up");
        drive_up(&mut store, "up-1", "demo", &["app"]);
        store
            .begin_down("down", "demo", &["app".into()])
            .expect("down");
        store
            .mark_stop_invoked("down", "demo", "app")
            .expect("stop invoked");
        store
            .mark_stopped("down", "demo", "app", Some(0), None)
            .expect("stopped");
        store.finish_down("down", "demo").expect("finish down");

        let mut candidate = initial;
        candidate
            .services
            .get_mut("app")
            .expect("candidate service")
            .image = "alpine:3.23".into();
        let source = "candidate-source".to_owned();
        let base = PathBuf::from("/candidate");
        let candidate_plans = plans(&prefix.join("candidate"), &["app"]);
        let alias = candidate_plans["app"].alias.clone();
        store
            .begin_up("up-2", &source, &base, &candidate, &candidate_plans)
            .expect("candidate up");
        store
            .mark_logs_prepared("up-2", "demo", "app")
            .expect("candidate logs");
        store
            .mark_install_invoked("up-2", "demo", "app")
            .expect("candidate install intent");
        if installed {
            store
                .mark_installed("up-2", "demo", "app")
                .expect("candidate installed");
        }
        store
            .mark_service_failed(
                "up-2",
                "demo",
                "app",
                "candidate_failure",
                "candidate failed before start",
                false,
            )
            .expect("candidate failure");
        store
            .finalize_operation_failure(
                "up-2",
                "demo",
                "candidate_failure",
                "candidate failed before start",
            )
            .expect("finalize candidate failure");
        (prefix, store, candidate, source, base, alias)
    }

    #[test]
    fn initializes_and_reopens_the_database() {
        let prefix = crate::paths::test_prefix("store-v3");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let first = Store::open(&paths.database_path()).expect("open store");
        let installation_id = first.installation_id().to_owned();
        assert_eq!(installation_id.len(), 32);
        assert!(first.stack_status("missing").expect("status").is_none());
        let read_only = crate::protocol::Response::success(
            "status-without-operation",
            serde_json::json!({"observed_state": "missing"}),
        );
        first
            .cache_response("status-without-operation", &read_only)
            .expect("read-only response cache is optional");
        drop(first);
        let second = Store::open(&paths.database_path()).expect("reopen store");
        assert_eq!(second.installation_id(), installation_id);
        drop(second);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn replay_preserves_raw_up_inputs_and_exact_target() {
        let prefix = crate::paths::test_prefix("store-replay-inputs");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "initial",
                "initial-source",
                Path::new("/initial-base"),
                &manifest,
                &plans(&prefix.join("initial"), &["app"]),
            )
            .expect("initial up");
        drive_up(&mut store, "initial", "demo", &["app"]);

        store
            .record_noop_up("noop", "demo", "noop-source", Path::new("/noop-base"))
            .expect("record noop");
        let noop = store.operation_replay("noop").unwrap().unwrap();
        assert_eq!(noop.operation, "up");
        assert_eq!(noop.stack_name, "demo");
        assert_eq!(noop.candidate_manifest.as_deref(), Some("noop-source"));
        assert_eq!(noop.manifest_base.as_deref(), Some("/noop-base"));
        assert!(noop.target_service.is_none());
        assert!(noop.response_json.is_none());

        store
            .begin_down("down", "demo", &["app".into()])
            .expect("down");
        store
            .mark_stop_invoked("down", "demo", "app")
            .expect("stop invoked");
        store
            .mark_stopped("down", "demo", "app", Some(0), None)
            .expect("stopped");
        store.finish_down("down", "demo").expect("finish down");
        store
            .begin_start_current(
                "current-up",
                "demo",
                "up",
                &["app".into()],
                None,
                Some("current-source"),
                Some(Path::new("/current-base")),
            )
            .expect("journal current up");
        let current = store.operation_replay("current-up").unwrap().unwrap();
        assert_eq!(
            current.candidate_manifest.as_deref(),
            Some("current-source")
        );
        assert_eq!(current.manifest_base.as_deref(), Some("/current-base"));
        store
            .mark_start_invoked("current-up", "demo", "app")
            .unwrap();
        store
            .mark_starting("current-up", "demo", "app", 101, 201, "boot")
            .unwrap();
        store
            .mark_running("current-up", "demo", "app", 101)
            .unwrap();
        store.finish_start_current("current-up", "demo").unwrap();

        store
            .begin_restart("restart", "demo", "app")
            .expect("journal restart");
        let restart = store.operation_replay("restart").unwrap().unwrap();
        assert_eq!(restart.target_service.as_deref(), Some("app"));
        assert!(restart.candidate_manifest.is_none());
        assert!(restart.manifest_base.is_none());

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn manages_two_stacks_with_two_exact_services_each() {
        let prefix = crate::paths::test_prefix("store-multi");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");

        for (request, stack) in [("up-a", "alpha"), ("up-b", "beta")] {
            let manifest = manifest(stack, &["api", "db"]);
            store
                .begin_up(
                    request,
                    "source",
                    Path::new("/manifest"),
                    &manifest,
                    &plans(&prefix.join(stack), &["api", "db"]),
                )
                .expect("begin up");
            drive_up(&mut store, request, stack, &["api", "db"]);
        }
        let all = store.stack_statuses().expect("all statuses");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "alpha");
        assert_eq!(
            all[0]
                .services
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>(),
            vec!["api", "db"]
        );
        assert!(all.iter().all(|stack| stack.observed_state == "running"));

        store
            .begin_down("down-a", "alpha", &["db".into(), "api".into()])
            .expect("begin down");
        for name in ["db", "api"] {
            store
                .mark_stop_invoked("down-a", "alpha", name)
                .expect("stop intent");
            store
                .mark_stopped("down-a", "alpha", name, Some(0), None)
                .expect("stopped");
        }
        store.finish_down("down-a", "alpha").expect("finish down");
        assert_eq!(
            store.stack_status("alpha").unwrap().unwrap().observed_state,
            "stopped"
        );
        assert_eq!(
            store.stack_status("beta").unwrap().unwrap().observed_state,
            "running"
        );

        store
            .begin_start_current(
                "resume-a",
                "alpha",
                "up",
                &["api".into(), "db".into()],
                None,
                Some("source"),
                Some(Path::new("/manifest")),
            )
            .expect("journal current generations");
        for name in ["api", "db"] {
            store
                .mark_start_invoked("resume-a", "alpha", name)
                .expect("start intent");
            store
                .mark_starting("resume-a", "alpha", name, 300, 400, "boot")
                .expect("starting");
            store
                .mark_running("resume-a", "alpha", name, 300)
                .expect("running");
        }
        store
            .finish_start_current("resume-a", "alpha")
            .expect("finish current start");
        store
            .record_graceful_stop("alpha", "api", Some(0), None)
            .expect("proven stop");
        assert_eq!(
            store.resumable_services("alpha").expect("resumable"),
            vec!["api"]
        );
        let alpha = store.stack_status("alpha").unwrap().unwrap();
        let api = alpha
            .services
            .iter()
            .find(|service| service.name == "api")
            .unwrap();
        assert_eq!(api.desired_state, "running");
        assert_eq!(api.effect_phase, "shutdown_proven");
        assert_eq!(
            store.resumable_services("beta").unwrap(),
            Vec::<String>::new()
        );

        let identity = store
            .service_identity("beta", "api")
            .expect("owned beta service");
        store
            .begin_restart("restart-b", "beta", "api")
            .expect("restart intent");
        let journaled = store
            .service_identity("beta", "api")
            .expect("identity after intent");
        assert_eq!(journaled.child_pid, identity.child_pid);
        assert_eq!(journaled.session_id, identity.session_id);
        store
            .mark_restart_stop_invoked("restart-b", "beta", "api")
            .expect("stop invoked");
        let stopping = store
            .service_identity("beta", "api")
            .expect("identity while stopping");
        assert_eq!(stopping.child_pid, identity.child_pid);
        store
            .mark_restart_stopped("restart-b", "beta", "api", Some(0), None)
            .expect("qualified stop");
        let stopped = store
            .service_identity("beta", "api")
            .expect("current generation remains addressable");
        assert!(stopped.child_pid.is_none());
        assert!(stopped.session_id.is_none());
        store
            .mark_start_invoked("restart-b", "beta", "api")
            .expect("restart start invoked");
        store
            .mark_starting("restart-b", "beta", "api", 500, 600, "boot")
            .expect("restart starting");
        store
            .mark_running("restart-b", "beta", "api", 500)
            .expect("restart running");
        store
            .finish_restart("restart-b", "beta", "api")
            .expect("finish restart");
        assert_eq!(
            store
                .stack_status("beta")
                .unwrap()
                .unwrap()
                .services
                .into_iter()
                .find(|service| service.name == "api")
                .unwrap()
                .observed_state,
            "running"
        );

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn restart_crash_phases_preserve_only_proven_absence() {
        for phase in [
            "restart_stop_intent",
            "restart_stop_invoked",
            "shutdown_proven",
            "start_invoked",
        ] {
            let prefix = crate::paths::test_prefix(&format!("restart-crash-{phase}"));
            let paths = RuntimePaths::new(prefix.clone());
            paths.prepare().expect("prepare paths");
            let mut store = Store::open(&paths.database_path()).expect("open store");
            let manifest = manifest("demo", &["app"]);
            store
                .begin_up(
                    "up",
                    "source",
                    Path::new("/manifest"),
                    &manifest,
                    &plans(&prefix.join(phase), &["app"]),
                )
                .expect("begin up");
            drive_up(&mut store, "up", "demo", &["app"]);
            store
                .begin_restart("restart", "demo", "app")
                .expect("restart intent");
            if phase != "restart_stop_intent" {
                store
                    .mark_restart_stop_invoked("restart", "demo", "app")
                    .expect("stop invoked");
            }
            if matches!(phase, "shutdown_proven" | "start_invoked") {
                store
                    .mark_restart_stopped("restart", "demo", "app", Some(0), None)
                    .expect("stop proven");
            }
            if phase == "start_invoked" {
                store
                    .mark_start_invoked("restart", "demo", "app")
                    .expect("start invoked");
            }

            store.reconcile_cold_start().expect("cold reconcile");
            let status = store.stack_status("demo").unwrap().unwrap();
            let service = &status.services[0];
            assert_eq!(service.rootfs_state, "installed", "phase={phase}");
            if phase == "shutdown_proven" {
                assert_eq!(service.observed_state, "stopped");
                assert_eq!(service.effect_phase, "shutdown_proven");
                assert_eq!(store.resumable_services("demo").unwrap(), vec!["app"]);
            } else {
                assert_eq!(service.observed_state, "unknown", "phase={phase}");
                assert!(store.resumable_services("demo").unwrap().is_empty());
            }

            drop(store);
            fs::remove_dir_all(prefix).expect("remove test prefix");
        }
    }

    #[test]
    fn cold_start_preserves_backoff_and_down_cancels_it_without_a_child() {
        let prefix = crate::paths::test_prefix("backoff-cold-down");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/manifest"),
                &manifest,
                &plans(&prefix.join("demo"), &["app"]),
            )
            .expect("begin up");
        drive_up(&mut store, "up", "demo", &["app"]);
        store
            .record_exit("demo", "app", Some(7), None)
            .expect("direct exit proves absence");
        store
            .schedule_restart("demo", "app", 2, 8_000_000_000, 9_000_000_000)
            .expect("persist backoff");
        drop(store);

        let mut store = Store::open(&paths.database_path()).expect("reopen store");
        assert_eq!(store.reconcile_cold_start().expect("cold reconcile"), 0);
        let status = store.stack_status("demo").unwrap().unwrap();
        let service = &status.services[0];
        assert_eq!(status.observed_state, "restarting");
        assert_eq!(service.observed_state, "restarting");
        assert_eq!(service.effect_phase, "backoff");
        assert_eq!(service.next_restart_at, Some(9_000_000_000));
        assert!(store.resumable_services("demo").unwrap().is_empty());
        let due = store.due_restarts(9_000_000_000).expect("due restart");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].service_name, "app");
        assert_eq!(due[0].attempts, 2);

        let before_shutdown = store.existing_stack("demo").unwrap().unwrap().services[0].clone();
        store
            .record_graceful_backoff_stop("demo", "app")
            .expect("gracefully cancel proven backoff");
        let after_shutdown = store.existing_stack("demo").unwrap().unwrap().services[0].clone();
        assert_eq!(after_shutdown.observed_state, "stopped");
        assert_eq!(after_shutdown.effect_phase, "shutdown_proven");
        assert!(after_shutdown.next_restart_at.is_none());
        assert_eq!(
            after_shutdown.restart_attempts,
            before_shutdown.restart_attempts
        );
        assert_eq!(
            after_shutdown.restart_window_started_at,
            before_shutdown.restart_window_started_at
        );
        assert_eq!(store.resumable_services("demo").unwrap(), vec!["app"]);
        assert!(matches!(
            store.record_graceful_backoff_stop("demo", "app"),
            Err(Error::NotFound(_))
        ));

        store
            .begin_down("down", "demo", &["app".into()])
            .expect("journal down");
        let cancelled = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(cancelled.observed_state, "stopped");
        assert_eq!(cancelled.services[0].observed_state, "stopped");
        assert_eq!(cancelled.services[0].effect_phase, "stopped");
        assert!(cancelled.services[0].next_restart_at.is_none());
        assert!(store.due_restarts(i64::MAX).unwrap().is_empty());
        store
            .finish_down("down", "demo")
            .expect("no child stop required");

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn deferring_restart_only_moves_the_deadline_and_preserves_accounting() {
        let prefix = crate::paths::test_prefix("restart-defer");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/manifest"),
                &manifest,
                &plans(&prefix.join("demo"), &["app"]),
            )
            .expect("begin up");
        drive_up(&mut store, "up", "demo", &["app"]);
        store
            .record_exit("demo", "app", Some(7), None)
            .expect("proven exit");
        store
            .schedule_restart("demo", "app", 3, 10_000, 20_000)
            .expect("schedule restart");
        let before = store.existing_stack("demo").unwrap().unwrap().services[0].clone();

        store
            .defer_restart("demo", "app", 30_000)
            .expect("move restart deadline forward");
        let after = store.existing_stack("demo").unwrap().unwrap().services[0].clone();
        assert_eq!(after.restart_attempts, before.restart_attempts);
        assert_eq!(
            after.restart_window_started_at,
            before.restart_window_started_at
        );
        assert_eq!(after.next_restart_at, Some(30_000));
        assert!(store.due_restarts(29_999).unwrap().is_empty());
        assert_eq!(store.due_restarts(30_000).unwrap().len(), 1);
        assert!(matches!(
            store.defer_restart("demo", "app", 30_000),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            store.defer_restart("demo", "app", 25_000),
            Err(Error::NotFound(_))
        ));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn restoring_restart_backoff_requires_proven_absence_and_preserves_accounting() {
        let prefix = crate::paths::test_prefix("restart-restore");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/manifest"),
                &manifest,
                &plans(&prefix.join("demo"), &["app"]),
            )
            .expect("begin up");
        drive_up(&mut store, "up", "demo", &["app"]);
        store
            .record_exit("demo", "app", Some(7), None)
            .expect("proven exit");
        store
            .schedule_restart("demo", "app", 3, 10_000, 20_000)
            .expect("schedule restart");
        store
            .mark_restart_invoked("demo", "app")
            .expect("restart invoked");
        store
            .restore_restart_backoff("demo", "app", 30_000)
            .expect("restore pre-spawn backoff");
        let restored = store.existing_stack("demo").unwrap().unwrap().services[0].clone();
        assert_eq!(restored.observed_state, "restarting");
        assert_eq!(restored.effect_phase, "backoff");
        assert_eq!(restored.restart_attempts, 3);
        assert_eq!(restored.restart_window_started_at, Some(10_000));
        assert_eq!(restored.next_restart_at, Some(30_000));
        assert!(matches!(
            store.restore_restart_backoff("demo", "app", 40_000),
            Err(Error::NotFound(_))
        ));

        store
            .mark_restart_invoked("demo", "app")
            .expect("second restart invoked");
        store
            .connection
            .execute(
                "UPDATE services SET child_pid = 777
                  WHERE stack_name = 'demo' AND name = 'app'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.restore_restart_backoff("demo", "app", 40_000),
            Err(Error::NotFound(_))
        ));
        store
            .connection
            .execute(
                "UPDATE services
                    SET child_pid = NULL, observed_state = 'unknown', effect_phase = 'unknown'
                  WHERE stack_name = 'demo' AND name = 'app'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.restore_restart_backoff("demo", "app", 40_000),
            Err(Error::NotFound(_))
        ));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn runtime_unknown_preserves_identity_and_down_does_not_claim_absence() {
        let prefix = crate::paths::test_prefix("runtime-unknown");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/manifest"),
                &manifest,
                &plans(&prefix.join("demo"), &["app"]),
            )
            .expect("begin up");
        drive_up(&mut store, "up", "demo", &["app"]);
        let identity = store.service_identity("demo", "app").expect("identity");

        store
            .mark_runtime_unknown("demo", "app")
            .expect("ambiguous supervision");
        let preserved = store
            .service_identity("demo", "app")
            .expect("preserved identity");
        assert_eq!(preserved.session_id, identity.session_id);
        assert_eq!(preserved.child_pid, identity.child_pid);
        assert_eq!(preserved.child_starttime, identity.child_starttime);
        assert_eq!(preserved.boot_id, identity.boot_id);
        assert_eq!(
            store.stack_status("demo").unwrap().unwrap().observed_state,
            "unknown"
        );

        store
            .begin_down("down", "demo", &["app".into()])
            .expect("journal unknown down");
        let status = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(status.desired_state, "stopped");
        assert_eq!(status.observed_state, "unknown");
        assert_eq!(status.services[0].observed_state, "unknown");
        assert!(matches!(
            store.mark_stopped("down", "demo", "app", Some(0), None),
            Err(Error::Conflict(_))
        ));
        let operation_outcome: String = store
            .connection
            .query_row(
                "SELECT outcome FROM operations WHERE request_id = 'down'",
                [],
                |row| row.get(0),
            )
            .expect("failed unknown down");
        assert_eq!(operation_outcome, "failure");

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn down_failure_preserves_proven_stops_and_unknowns_only_remaining_services() {
        let prefix = crate::paths::test_prefix("down-online-failure");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["a", "b"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/manifest"),
                &manifest,
                &plans(&prefix.join("demo"), &["a", "b"]),
            )
            .expect("begin up");
        drive_up(&mut store, "up", "demo", &["a", "b"]);
        let remaining_identity = store.service_identity("demo", "a").expect("a identity");

        store
            .begin_down("down", "demo", &["b".into(), "a".into()])
            .expect("begin down");
        store
            .mark_stop_invoked("down", "demo", "b")
            .expect("b stop invoked");
        store
            .mark_stopped("down", "demo", "b", Some(0), None)
            .expect("b stopped");
        store
            .finalize_down_failure(
                "down",
                "demo",
                "engine_stop",
                "could not stop the remaining service",
            )
            .expect("terminalize failed down");

        let status = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(status.desired_state, "stopped");
        assert_eq!(status.observed_state, "unknown");
        let by_name = status
            .services
            .iter()
            .map(|service| (service.name.as_str(), service))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_name["b"].observed_state, "stopped");
        assert_eq!(by_name["b"].effect_phase, "stopped");
        assert_eq!(by_name["a"].desired_state, "stopped");
        assert_eq!(by_name["a"].observed_state, "unknown");
        assert_eq!(by_name["a"].effect_phase, "unknown");
        let preserved = store
            .service_identity("demo", "a")
            .expect("remaining identity is retained");
        assert_eq!(preserved.session_id, remaining_identity.session_id);
        assert_eq!(preserved.child_pid, remaining_identity.child_pid);
        assert_eq!(
            preserved.child_starttime,
            remaining_identity.child_starttime
        );
        assert_eq!(preserved.boot_id, remaining_identity.boot_id);

        let rows = store
            .connection
            .prepare(
                "SELECT service_name, phase, outcome FROM operation_services
                  WHERE request_id = 'down' ORDER BY service_name",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a".into(), "unknown".into(), "failure".into()),
                ("b".into(), "stopped".into(), "success".into()),
            ]
        );
        let parent: (String, String, String) = store
            .connection
            .query_row(
                "SELECT phase, outcome, error_code FROM operations WHERE request_id = 'down'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            parent,
            ("unknown".into(), "failure".into(), "engine_stop".into())
        );
        assert!(matches!(
            store.finalize_down_failure("down", "demo", "again", "already terminal"),
            Err(Error::NotFound(_))
        ));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn failed_candidate_never_shadows_committed_identity() {
        let prefix = crate::paths::test_prefix("candidate-identity");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up-1",
                "source-1",
                Path::new("/manifest"),
                &manifest,
                &plans(&prefix.join("first"), &["app"]),
            )
            .expect("first up");
        drive_up(&mut store, "up-1", "demo", &["app"]);
        let committed = store
            .service_identity("demo", "app")
            .expect("committed identity");
        store
            .begin_down("down", "demo", &["app".into()])
            .expect("down");
        store
            .mark_stop_invoked("down", "demo", "app")
            .expect("stop invoked");
        store
            .mark_stopped("down", "demo", "app", Some(0), None)
            .expect("stopped");
        store.finish_down("down", "demo").expect("finish down");

        let candidate_plans = plans(&prefix.join("second"), &["app"]);
        let candidate_alias = candidate_plans["app"].alias.clone();
        store
            .begin_up(
                "up-2",
                "source-2",
                Path::new("/manifest"),
                &manifest,
                &candidate_plans,
            )
            .expect("candidate up");
        assert_eq!(
            store.service_identity("demo", "app").unwrap().alias,
            candidate_alias
        );
        store
            .mark_service_failed(
                "up-2",
                "demo",
                "app",
                "test_failure",
                "candidate failed",
                false,
            )
            .expect("fail candidate");
        let while_cleanup_is_pending = store
            .service_identity("demo", "app")
            .expect("candidate identity during cleanup");
        assert_eq!(while_cleanup_is_pending.alias, candidate_alias);
        assert_eq!(
            store.stack_status("demo").unwrap().unwrap().services[0]
                .alias
                .as_deref(),
            Some(candidate_alias.as_str())
        );
        let parent_outcome: Option<String> = store
            .connection
            .query_row(
                "SELECT outcome FROM operations WHERE request_id = 'up-2'",
                [],
                |row| row.get(0),
            )
            .expect("pending parent outcome");
        assert!(parent_outcome.is_none());
        store
            .finalize_operation_failure("up-2", "demo", "test_failure", "candidate failed")
            .expect("finalize after cleanup");
        let after_failure = store
            .service_identity("demo", "app")
            .expect("committed identity after finalization");
        assert_eq!(after_failure.alias, committed.alias);
        assert_eq!(after_failure.generation, committed.generation);
        assert_eq!(
            store.stack_status("demo").unwrap().unwrap().services[0]
                .alias
                .as_deref(),
            Some(committed.alias.as_str())
        );

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn retries_an_exact_fully_installed_candidate_without_new_generation() {
        let (prefix, mut store, candidate, source, base, alias) =
            failed_candidate_fixture("candidate-retry", true);
        let generations_before: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM rootfs_generations WHERE stack_name = 'demo'",
                [],
                |row| row.get(0),
            )
            .expect("generation count");

        let recovery = store
            .begin_candidate_recovery(
                "up-3",
                &source,
                &base,
                &candidate,
                &plans(&prefix.join("fresh-unused"), &["app"]),
            )
            .expect("reuse installed candidate");
        assert_eq!(recovery.revision, 2);
        assert_eq!(recovery.services["app"].plan.alias, alias);
        assert!(recovery.services["app"].reuse_installed);
        assert_eq!(store.service_identity("demo", "app").unwrap().alias, alias);
        let generations_after: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM rootfs_generations WHERE stack_name = 'demo'",
                [],
                |row| row.get(0),
            )
            .expect("generation count");
        assert_eq!(generations_after, generations_before);
        let phase: String = store
            .connection
            .query_row(
                "SELECT phase FROM operation_services WHERE request_id = 'up-3'",
                [],
                |row| row.get(0),
            )
            .expect("retry phase");
        assert_eq!(phase, "installed");

        store
            .mark_start_invoked("up-3", "demo", "app")
            .expect("start invoked");
        store
            .mark_starting("up-3", "demo", "app", 700, 800, "boot")
            .expect("starting");
        store
            .mark_running("up-3", "demo", "app", 700)
            .expect("running");
        store.commit_up("up-3", "demo").expect("commit retry");
        let committed = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(committed.revision, 2);
        assert_eq!(committed.services[0].alias.as_deref(), Some(alias.as_str()));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn candidate_recovery_uses_latest_generations_not_same_second_request_order() {
        let prefix = crate::paths::test_prefix("candidate-generation-order");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let initial = manifest("demo", &["app"]);
        store
            .begin_up(
                "initial",
                "initial-source",
                Path::new("/initial"),
                &initial,
                &plans(&prefix.join("initial"), &["app"]),
            )
            .unwrap();
        drive_up(&mut store, "initial", "demo", &["app"]);
        store.begin_down("down", "demo", &["app".into()]).unwrap();
        store.mark_stop_invoked("down", "demo", "app").unwrap();
        store
            .mark_stopped("down", "demo", "app", Some(0), None)
            .unwrap();
        store.finish_down("down", "demo").unwrap();

        let mut candidate = initial;
        candidate.services.get_mut("app").unwrap().image = "alpine:3.23".into();
        let source = "candidate-source";
        let base = Path::new("/candidate");
        store
            .begin_up(
                "z-stale",
                source,
                base,
                &candidate,
                &plans(&prefix.join("stale"), &["app"]),
            )
            .unwrap();
        store.mark_logs_prepared("z-stale", "demo", "app").unwrap();
        store
            .mark_service_failed(
                "z-stale",
                "demo",
                "app",
                "pre_effect",
                "failed before install",
                false,
            )
            .unwrap();
        store
            .finalize_operation_failure("z-stale", "demo", "pre_effect", "stale failed")
            .unwrap();

        let latest = store
            .begin_candidate_recovery(
                "a-latest",
                source,
                base,
                &candidate,
                &plans(&prefix.join("latest"), &["app"]),
            )
            .expect("replace pre-effect generation");
        let latest_alias = latest.services["app"].plan.alias.clone();
        assert!(!latest.services["app"].reuse_installed);
        store.mark_logs_prepared("a-latest", "demo", "app").unwrap();
        store
            .mark_install_invoked("a-latest", "demo", "app")
            .unwrap();
        store.mark_installed("a-latest", "demo", "app").unwrap();
        store
            .mark_service_failed(
                "a-latest",
                "demo",
                "app",
                "start_failed",
                "installed candidate failed",
                false,
            )
            .unwrap();
        store
            .finalize_operation_failure("a-latest", "demo", "start_failed", "latest failed")
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE operations SET updated_at = 123
                  WHERE request_id IN ('z-stale', 'a-latest')",
                [],
            )
            .unwrap();

        let retry = store
            .begin_candidate_recovery(
                "retry",
                source,
                base,
                &candidate,
                &plans(&prefix.join("unused"), &["app"]),
            )
            .expect("select operation owning the latest generation");
        assert!(retry.services["app"].reuse_installed);
        assert_eq!(retry.services["app"].plan.alias, latest_alias);

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn recovers_installed_services_and_replaces_only_pre_effect_candidates() {
        let prefix = crate::paths::test_prefix("candidate-partial-recovery");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let initial = manifest("demo", &["a", "b"]);
        store
            .begin_up(
                "up-1",
                "initial",
                Path::new("/initial"),
                &initial,
                &plans(&prefix.join("initial"), &["a", "b"]),
            )
            .unwrap();
        drive_up(&mut store, "up-1", "demo", &["a", "b"]);
        store
            .begin_down("down", "demo", &["b".into(), "a".into()])
            .unwrap();
        for service in ["b", "a"] {
            store.mark_stop_invoked("down", "demo", service).unwrap();
            store
                .mark_stopped("down", "demo", service, Some(0), None)
                .unwrap();
        }
        store.finish_down("down", "demo").unwrap();

        let mut candidate = initial;
        for service in candidate.services.values_mut() {
            service.image = "alpine:3.23".into();
        }
        let source = "candidate-source";
        let base = Path::new("/candidate-base");
        let candidate_plans = plans(&prefix.join("candidate"), &["a", "b"]);
        let installed_alias = candidate_plans["a"].alias.clone();
        let replaced_alias = candidate_plans["b"].alias.clone();
        store
            .begin_up("up-2", source, base, &candidate, &candidate_plans)
            .unwrap();
        store.mark_logs_prepared("up-2", "demo", "a").unwrap();
        store.mark_install_invoked("up-2", "demo", "a").unwrap();
        store.mark_installed("up-2", "demo", "a").unwrap();
        store
            .mark_service_failed("up-2", "demo", "a", "start", "not started", false)
            .unwrap();
        store.mark_logs_prepared("up-2", "demo", "b").unwrap();
        store
            .mark_service_failed(
                "up-2",
                "demo",
                "b",
                "pre_effect",
                "failed before install",
                false,
            )
            .unwrap();
        store
            .finalize_operation_failure("up-2", "demo", "candidate", "candidate failed")
            .unwrap();

        let generations_before: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM rootfs_generations WHERE stack_name = 'demo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let fresh = plans(&prefix.join("fresh"), &["a", "b"]);
        let mut reused_alias = fresh.clone();
        reused_alias.get_mut("b").unwrap().alias = installed_alias.clone();
        assert!(matches!(
            store.begin_candidate_recovery("alias-reuse", source, base, &candidate, &reused_alias,),
            Err(Error::Conflict(_))
        ));

        let recovery = store
            .begin_candidate_recovery("up-3", source, base, &candidate, &fresh)
            .expect("recover exact partial candidate");
        assert_eq!(recovery.revision, 2);
        assert!(recovery.services["a"].reuse_installed);
        assert!(!recovery.services["b"].reuse_installed);
        assert_eq!(recovery.services["a"].plan.alias, installed_alias);
        assert_eq!(recovery.services["b"].plan.alias, fresh["b"].alias);
        let generations_after: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM rootfs_generations WHERE stack_name = 'demo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generations_after, generations_before + 1);
        let retired: (String, String) = store
            .connection
            .query_row(
                "SELECT state, role FROM rootfs_generations WHERE alias = ?1",
                [&replaced_alias],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retired, ("absent".into(), "retired".into()));
        let phases = store
            .connection
            .prepare(
                "SELECT service_name, phase FROM operation_services
                  WHERE request_id = 'up-3' ORDER BY service_name",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            phases,
            vec![
                ("a".into(), "installed".into()),
                ("b".into(), "intent".into())
            ]
        );

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn never_committed_stack_soft_retires_changed_services_and_can_readd_them() {
        let prefix = crate::paths::test_prefix("revision-zero-membership");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let first = manifest("demo", &["a", "b"]);
        let first_plans = plans(&prefix.join("first"), &["a", "b"]);
        let retired_alias = first_plans["a"].alias.clone();
        store
            .begin_up(
                "first-failure",
                "first-source",
                Path::new("/first"),
                &first,
                &first_plans,
            )
            .unwrap();
        for service in ["a", "b"] {
            store
                .mark_logs_prepared("first-failure", "demo", service)
                .unwrap();
            store
                .mark_install_invoked("first-failure", "demo", service)
                .unwrap();
            store
                .mark_installed("first-failure", "demo", service)
                .unwrap();
            store
                .mark_service_failed(
                    "first-failure",
                    "demo",
                    service,
                    "start_failed",
                    "candidate never started",
                    false,
                )
                .unwrap();
        }
        store
            .finalize_operation_failure(
                "first-failure",
                "demo",
                "start_failed",
                "initial candidate failed",
            )
            .unwrap();
        assert_eq!(store.stack_status("demo").unwrap().unwrap().revision, 0);

        let corrected = manifest("demo", &["b", "c"]);
        let corrected_plans = plans(&prefix.join("corrected"), &["b", "c"]);
        assert!(matches!(
            store.begin_candidate_recovery(
                "corrected-probe",
                "corrected-source",
                Path::new("/corrected"),
                &corrected,
                &corrected_plans,
            ),
            Err(Error::NotFound(_))
        ));
        store
            .begin_up(
                "corrected-up",
                "corrected-source",
                Path::new("/corrected"),
                &corrected,
                &corrected_plans,
            )
            .expect("replace a never-committed logical set");
        let corrected_status = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(
            corrected_status
                .services
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        let memberships = store
            .connection
            .prepare("SELECT name, active FROM services WHERE stack_name = 'demo' ORDER BY name")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            memberships,
            vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 1)]
        );
        let retained: (String, String, String) = store
            .connection
            .query_row(
                "SELECT service_name, state, role FROM rootfs_generations WHERE alias = ?1",
                [&retired_alias],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("retired generation ownership");
        assert_eq!(retained, ("a".into(), "installed".into(), "retired".into()));

        for service in ["b", "c"] {
            store
                .mark_service_failed(
                    "corrected-up",
                    "demo",
                    service,
                    "corrected_failed",
                    "failed before install",
                    false,
                )
                .unwrap();
        }
        store
            .finalize_operation_failure(
                "corrected-up",
                "demo",
                "corrected_failed",
                "corrected candidate failed",
            )
            .unwrap();

        let readded = manifest("demo", &["a", "c"]);
        let readded_plans = plans(&prefix.join("readded"), &["a", "c"]);
        assert!(matches!(
            store.begin_candidate_recovery(
                "readd-probe",
                "readded-source",
                Path::new("/readded"),
                &readded,
                &readded_plans,
            ),
            Err(Error::NotFound(_))
        ));
        store
            .begin_up(
                "readded-up",
                "readded-source",
                Path::new("/readded"),
                &readded,
                &readded_plans,
            )
            .expect("re-add a soft-retired logical service");
        let readded_status = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(
            readded_status
                .services
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
        let retained_count: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM rootfs_generations
                  WHERE stack_name = 'demo' AND service_name = 'a' AND alias = ?1",
                [&retired_alias],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained_count, 1);

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn never_committed_effectful_candidate_blocks_service_set_replacement() {
        let prefix = crate::paths::test_prefix("revision-zero-effectful");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let first = manifest("demo", &["a"]);
        store
            .begin_up(
                "effectful",
                "first-source",
                Path::new("/first"),
                &first,
                &plans(&prefix.join("first"), &["a"]),
            )
            .unwrap();
        store.mark_logs_prepared("effectful", "demo", "a").unwrap();
        store
            .mark_install_invoked("effectful", "demo", "a")
            .unwrap();
        store
            .mark_service_failed(
                "effectful",
                "demo",
                "a",
                "install_failed",
                "install invocation may have created a rootfs",
                false,
            )
            .unwrap();
        store
            .finalize_operation_failure(
                "effectful",
                "demo",
                "install_failed",
                "effectful candidate failed",
            )
            .unwrap();

        let corrected = manifest("demo", &["b"]);
        let fresh = plans(&prefix.join("corrected"), &["b"]);
        assert!(matches!(
            store.begin_candidate_recovery(
                "probe",
                "corrected-source",
                Path::new("/corrected"),
                &corrected,
                &fresh,
            ),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            store.begin_up(
                "corrected",
                "corrected-source",
                Path::new("/corrected"),
                &corrected,
                &fresh,
            ),
            Err(Error::Conflict(_))
        ));
        let retained: (i64, String, String) = store
            .connection
            .query_row(
                "SELECT v.active, r.state, r.role
                   FROM services AS v
                   JOIN rootfs_generations AS r
                     ON r.stack_name = v.stack_name AND r.service_name = v.name
                  WHERE v.stack_name = 'demo' AND v.name = 'a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(retained, (1, "preparing".into(), "candidate".into()));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn corrected_candidate_payload_falls_back_to_a_fresh_generation() {
        let (prefix, mut store, mut candidate, _source, _base, old_alias) =
            failed_candidate_fixture("candidate-corrected", true);
        candidate
            .services
            .get_mut("app")
            .expect("candidate service")
            .image = "alpine:3.24".into();
        let corrected_source = "corrected-candidate-source";
        let corrected_base = Path::new("/corrected-candidate");
        let fresh = plans(&prefix.join("corrected"), &["app"]);

        assert!(matches!(
            store.begin_candidate_recovery(
                "recovery-probe",
                corrected_source,
                corrected_base,
                &candidate,
                &fresh,
            ),
            Err(Error::NotFound(_))
        ));
        assert_eq!(
            store
                .begin_up("up-3", corrected_source, corrected_base, &candidate, &fresh,)
                .expect("corrected payload starts a fresh candidate"),
            2
        );
        let identity = store
            .service_identity("demo", "app")
            .expect("fresh candidate identity");
        assert_eq!(identity.alias, fresh["app"].alias);
        assert_ne!(identity.alias, old_alias);
        let old_role: String = store
            .connection
            .query_row(
                "SELECT role FROM rootfs_generations WHERE alias = ?1",
                [&old_alias],
                |row| row.get(0),
            )
            .expect("old candidate role");
        assert_eq!(old_role, "retired");

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn refuses_candidate_retry_on_mismatch_unknown_or_partial_install() {
        let (prefix, mut store, candidate, source, base, _) =
            failed_candidate_fixture("candidate-refusal", true);
        let fresh = plans(&prefix.join("fresh"), &["app"]);
        assert!(matches!(
            store.begin_candidate_recovery("wrong-source", "different", &base, &candidate, &fresh),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            store.begin_candidate_recovery(
                "wrong-base",
                &source,
                Path::new("/different"),
                &candidate,
                &fresh,
            ),
            Err(Error::NotFound(_))
        ));
        let mut extra_service = candidate.clone();
        extra_service
            .services
            .insert("worker".into(), service("worker", "alpine:3.23"));
        assert!(matches!(
            store.begin_candidate_recovery(
                "wrong-services",
                &source,
                &base,
                &extra_service,
                &fresh,
            ),
            Err(Error::Conflict(_))
        ));
        store
            .mark_runtime_unknown("demo", "app")
            .expect("unknown service");
        assert!(matches!(
            store.begin_candidate_recovery("unknown", &source, &base, &candidate, &fresh),
            Err(Error::Conflict(_))
        ));
        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");

        let (prefix, mut store, candidate, source, base, _) =
            failed_candidate_fixture("candidate-partial", false);
        let fresh = plans(&prefix.join("fresh"), &["app"]);
        assert!(matches!(
            store.begin_candidate_recovery(
                "partial-corrected",
                "corrected-source",
                &base,
                &candidate,
                &fresh,
            ),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            store.begin_candidate_recovery("partial", &source, &base, &candidate, &fresh),
            Err(Error::Conflict(_))
        ));
        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn cold_reconcile_is_per_service_and_per_stack() {
        let prefix = crate::paths::test_prefix("store-reconcile-v3");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");

        let stable = manifest("stable", &["app"]);
        store
            .begin_up(
                "stable-up",
                "stable",
                Path::new("/m"),
                &stable,
                &plans(&prefix, &["app"]),
            )
            .unwrap();
        drive_up(&mut store, "stable-up", "stable", &["app"]);
        store
            .begin_down("stable-down", "stable", &["app".into()])
            .unwrap();
        store
            .mark_stop_invoked("stable-down", "stable", "app")
            .unwrap();
        store
            .mark_stopped("stable-down", "stable", "app", Some(0), None)
            .unwrap();
        store.finish_down("stable-down", "stable").unwrap();
        store
            .begin_start_current(
                "stable-resume",
                "stable",
                "up",
                &["app".into()],
                None,
                Some("stable"),
                Some(Path::new("/m")),
            )
            .unwrap();

        let partial = manifest("partial", &["a", "b"]);
        store
            .begin_up(
                "partial-up",
                "partial",
                Path::new("/m"),
                &partial,
                &plans(&prefix, &["a", "b"]),
            )
            .unwrap();
        store
            .mark_logs_prepared("partial-up", "partial", "a")
            .unwrap();
        store
            .mark_install_invoked("partial-up", "partial", "a")
            .unwrap();
        store.mark_installed("partial-up", "partial", "a").unwrap();
        store
            .mark_start_invoked("partial-up", "partial", "a")
            .unwrap();
        store
            .mark_starting("partial-up", "partial", "a", 100, 200, "boot")
            .unwrap();
        store
            .mark_running("partial-up", "partial", "a", 100)
            .unwrap();
        store
            .mark_logs_prepared("partial-up", "partial", "b")
            .unwrap();
        store
            .mark_install_invoked("partial-up", "partial", "b")
            .unwrap();
        store.mark_installed("partial-up", "partial", "b").unwrap();

        assert_eq!(store.reconcile_cold_start().expect("reconcile"), 3);
        let partial = store.stack_status("partial").unwrap().unwrap();
        assert_eq!(partial.observed_state, "unknown");
        assert_eq!(partial.services[0].observed_state, "unknown");
        assert_eq!(partial.services[1].observed_state, "failed");
        let stable = store.stack_status("stable").unwrap().unwrap();
        assert_eq!(stable.services[0].observed_state, "stopped");
        assert_eq!(stable.services[0].effect_phase, "shutdown_proven");
        assert_eq!(stable.services[0].rootfs_state, "installed");
        assert_eq!(
            store.resumable_services("stable").expect("safe resume"),
            vec!["app"]
        );

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn cold_reconcile_projects_a_terminal_revision_zero_candidate() {
        let prefix = crate::paths::test_prefix("store-terminal-candidate-status");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("partial", &["seed", "web"]);
        let service_plans = plans(&prefix, &["seed", "web"]);
        let seed_alias = service_plans["seed"].alias.clone();

        store
            .begin_up(
                "partial-up",
                "partial",
                Path::new("/m"),
                &manifest,
                &service_plans,
            )
            .expect("begin up");
        store
            .mark_logs_prepared("partial-up", "partial", "seed")
            .expect("seed logs");
        store
            .mark_install_invoked("partial-up", "partial", "seed")
            .expect("seed install intent");
        store
            .mark_installed("partial-up", "partial", "seed")
            .expect("seed installed");
        store
            .mark_start_invoked("partial-up", "partial", "seed")
            .expect("seed start intent");
        store
            .mark_starting("partial-up", "partial", "seed", 100, 200, "boot")
            .expect("seed starting");
        store
            .mark_running("partial-up", "partial", "seed", 100)
            .expect("seed running");

        assert_eq!(store.reconcile_cold_start().expect("reconcile"), 2);
        let status = store.stack_status("partial").unwrap().unwrap();
        assert_eq!(status.revision, 0);
        assert_eq!(status.observed_state, "unknown");
        let by_name = status
            .services
            .iter()
            .map(|service| (service.name.as_str(), service))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(by_name["seed"].observed_state, "unknown");
        assert_eq!(by_name["seed"].rootfs_state, "installed");
        assert_eq!(by_name["seed"].alias.as_deref(), Some(seed_alias.as_str()));
        assert_eq!(by_name["web"].observed_state, "failed");
        assert_eq!(by_name["web"].rootfs_state, "absent");
        assert!(by_name["web"].alias.is_none());
        let parent: (String, String) = store
            .connection
            .query_row(
                "SELECT phase, outcome FROM operations WHERE request_id = 'partial-up'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("terminal parent");
        assert_eq!(parent, ("unknown".into(), "failure".into()));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn cold_reconcile_terminalizes_a_parent_without_child_rows() {
        let prefix = crate::paths::test_prefix("store-parent-only-reconcile");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/base"),
                &manifest,
                &plans(&prefix.join("initial"), &["app"]),
            )
            .unwrap();
        drive_up(&mut store, "up", "demo", &["app"]);
        store.begin_down("down", "demo", &["app".into()]).unwrap();
        store.mark_stop_invoked("down", "demo", "app").unwrap();
        store
            .mark_stopped("down", "demo", "app", Some(0), None)
            .unwrap();
        store.finish_down("down", "demo").unwrap();

        store
            .connection
            .execute(
                "INSERT INTO operations(
                     request_id, stack_name, operation, phase, candidate_manifest,
                     manifest_base, candidate_revision, target_service,
                     created_at, updated_at
                 ) VALUES ('parent-only', 'demo', 'up', 'intent', 'raw', '/base', 2,
                           NULL, 1, 1)",
                [],
            )
            .unwrap();
        assert_eq!(store.reconcile_cold_start().unwrap(), 0);
        let terminal: (String, String, String) = store
            .connection
            .query_row(
                "SELECT phase, outcome, error_code FROM operations
                  WHERE request_id = 'parent-only'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            terminal,
            ("failed".into(), "failure".into(), "interrupted".into())
        );
        store
            .begin_start_current(
                "after-parent-only",
                "demo",
                "up",
                &["app".into()],
                None,
                Some("raw"),
                Some(Path::new("/base")),
            )
            .expect("terminal parent does not wedge the stack");

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn cold_reconcile_finishes_an_all_proven_down_parent() {
        let prefix = crate::paths::test_prefix("store-proven-down-reconcile");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let manifest = manifest("demo", &["app"]);
        store
            .begin_up(
                "up",
                "source",
                Path::new("/base"),
                &manifest,
                &plans(&prefix.join("initial"), &["app"]),
            )
            .unwrap();
        drive_up(&mut store, "up", "demo", &["app"]);
        store.begin_down("down", "demo", &["app".into()]).unwrap();
        store.mark_stop_invoked("down", "demo", "app").unwrap();
        store
            .mark_stopped("down", "demo", "app", Some(0), None)
            .unwrap();

        assert_eq!(store.reconcile_cold_start().unwrap(), 0);
        let outcome: String = store
            .connection
            .query_row(
                "SELECT outcome FROM operations WHERE request_id = 'down'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outcome, "success");
        let status = store.stack_status("demo").unwrap().unwrap();
        assert_eq!(status.desired_state, "stopped");
        assert_eq!(status.observed_state, "stopped");

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn migrates_v2_without_claiming_live_ownership() {
        let prefix = crate::paths::test_prefix("store-migrate-v2");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let connection = rusqlite::Connection::open(paths.database_path()).expect("open SQLite");
        connection.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
             INSERT INTO meta VALUES ('installation_id', '0123456789abcdef0123456789abcdef');
             CREATE TABLE stacks (name TEXT PRIMARY KEY, desired_state TEXT NOT NULL, observed_state TEXT NOT NULL, manifest TEXT NOT NULL, revision TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL) STRICT;
             CREATE TABLE services (stack_name TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE, name TEXT NOT NULL, image TEXT NOT NULL, command_json TEXT NOT NULL, alias TEXT NOT NULL UNIQUE, observed_state TEXT NOT NULL, rootfs_state TEXT NOT NULL, session_id INTEGER, child_pid INTEGER, child_starttime INTEGER, boot_id TEXT, last_exit_code INTEGER, stdout_log_path TEXT NOT NULL, stderr_log_path TEXT NOT NULL, PRIMARY KEY (stack_name, name)) STRICT;
             CREATE TABLE operations (request_id TEXT PRIMARY KEY, stack_name TEXT NOT NULL, operation TEXT NOT NULL, phase TEXT NOT NULL, outcome TEXT, error_code TEXT, error_message TEXT, response_json TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL) STRICT;
             INSERT INTO stacks VALUES ('stopped', 'stopped', 'stopped', 'stopped-source', 'old', 1, 2);
             INSERT INTO services VALUES ('stopped', 'app', 'alpine', 'null', 'alias-stopped', 'stopped', 'installed', NULL, NULL, NULL, NULL, 0, '/out', '/err');
             INSERT INTO operations VALUES ('up-stopped', 'stopped', 'up', 'committed', 'success', NULL, NULL, NULL, 1, 1);
             INSERT INTO stacks VALUES ('active', 'running', 'running', 'active-source', 'old', 1, 2);
             INSERT INTO services VALUES ('active', 'app', 'alpine', 'null', 'alias-active', 'running', 'installed', 10, 10, 20, 'boot', NULL, '/out2', '/err2');
             INSERT INTO operations VALUES ('up-active', 'active', 'up', 'committed', 'success', NULL, NULL, NULL, 1, 1);
             INSERT INTO stacks VALUES ('never-committed', 'stopped', 'stopped', 'failed-source', 'old', 1, 2);
             INSERT INTO services VALUES ('never-committed', 'app', 'alpine', 'null', 'alias-never', 'stopped', 'installed', NULL, NULL, NULL, NULL, 17, '/out3', '/err3');
             INSERT INTO operations VALUES ('up-never', 'never-committed', 'up', 'failed', 'failure', 'start_failed', 'candidate failed', NULL, 1, 2);
             PRAGMA user_version = 2;",
        ).expect("create v2 fixture");
        drop(connection);
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(paths.database_path())
            .expect("database metadata")
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(paths.database_path(), permissions).expect("private database mode");

        let store = Store::open(&paths.database_path()).expect("migrate");
        let stopped = store.stack_status("stopped").unwrap().unwrap();
        assert_eq!(stopped.revision, 1);
        assert_eq!(stopped.observed_state, "stopped");
        assert_eq!(stopped.services[0].rootfs_state, "installed");
        assert_eq!(stopped.services[0].alias.as_deref(), Some("alias-stopped"));
        let active = store.stack_status("active").unwrap().unwrap();
        assert_eq!(active.observed_state, "unknown");
        assert_eq!(active.services[0].observed_state, "unknown");
        assert!(active.services[0].session_id.is_none());
        let never_committed = store.stack_status("never-committed").unwrap().unwrap();
        assert_eq!(never_committed.revision, 0);
        assert_eq!(never_committed.observed_state, "stopped");
        assert!(never_committed.services[0].alias.is_none());
        let retained: (String, i64, Option<String>, Option<i64>, String, String) = store
            .connection
            .query_row(
                "SELECT s.manifest, s.committed_revision,
                        v.current_alias, v.current_generation, r.state, r.role
                   FROM stacks AS s
                   JOIN services AS v ON v.stack_name = s.name
                   JOIN rootfs_generations AS r
                     ON r.stack_name = v.stack_name AND r.service_name = v.name
                  WHERE s.name = 'never-committed'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            retained,
            (
                String::new(),
                0,
                None,
                None,
                "installed".into(),
                "retired".into()
            )
        );
        let memberships: i64 = store
            .connection
            .query_row("SELECT sum(active) FROM services", [], |row| row.get(0))
            .unwrap();
        assert_eq!(memberships, 3);
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn preserves_an_unknown_schema_version() {
        let prefix = crate::paths::test_prefix("store-schema-v3");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let store = Store::open(&paths.database_path()).expect("open store");
        drop(store);
        let connection = rusqlite::Connection::open(paths.database_path()).expect("open SQLite");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .expect("enable WAL");
        assert_eq!(journal_mode, "wal");
        connection
            .pragma_update(None, "user_version", 99)
            .expect("set future schema");
        drop(connection);
        let before = fs::read(paths.database_path()).expect("read future schema before open");
        assert!(matches!(
            Store::open(&paths.database_path()),
            Err(Error::Schema(99))
        ));
        let after = fs::read(paths.database_path()).expect("read future schema after rejection");
        assert_eq!(after, before, "future schema bytes must remain untouched");
        let connection = rusqlite::Connection::open(paths.database_path()).expect("reopen SQLite");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal mode");
        assert_eq!(journal_mode, "wal");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read future schema version");
        assert_eq!(version, 99);
        drop(connection);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }
}
