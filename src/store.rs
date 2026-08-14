use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 2;

pub(crate) struct Store {
    connection: Connection,
    installation_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct StackStatus {
    pub(crate) name: String,
    pub(crate) desired_state: String,
    pub(crate) observed_state: String,
    pub(crate) revision: String,
    pub(crate) service_name: String,
    pub(crate) service_state: String,
    pub(crate) rootfs_state: String,
    pub(crate) alias: String,
    pub(crate) session_id: Option<i64>,
    pub(crate) last_exit_code: Option<i64>,
    pub(crate) stdout_log: String,
    pub(crate) stderr_log: String,
}

#[derive(Debug)]
pub(crate) struct ServiceIdentity {
    pub(crate) alias: String,
    pub(crate) session_id: Option<u32>,
    pub(crate) child_pid: Option<u32>,
    pub(crate) child_starttime: Option<i64>,
    pub(crate) boot_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct OperationReplay {
    pub(crate) operation: String,
    pub(crate) stack_name: String,
    pub(crate) response_json: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ExistingStack {
    pub(crate) source: String,
    pub(crate) service_name: String,
    pub(crate) alias: String,
    pub(crate) service_state: String,
    pub(crate) rootfs_state: String,
    pub(crate) stdout_log: PathBuf,
    pub(crate) stderr_log: PathBuf,
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
            Self::NotFound(name) => write!(formatter, "stack {name:?} does not exist"),
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
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;",
        )?;

        let version =
            connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
        if version == 0 {
            initialize_schema(&connection)?;
        } else if version != SCHEMA_VERSION {
            return Err(Error::Schema(version));
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
        self.connection
            .query_row(
                "SELECT s.name, s.desired_state, s.observed_state, s.revision,
                        v.name, v.observed_state, v.rootfs_state, v.alias, v.session_id,
                        v.last_exit_code, v.stdout_log_path, v.stderr_log_path
                   FROM stacks AS s
                   JOIN services AS v ON v.stack_name = s.name
                  WHERE s.name = ?1",
                [name],
                |row| {
                    Ok(StackStatus {
                        name: row.get(0)?,
                        desired_state: row.get(1)?,
                        observed_state: row.get(2)?,
                        revision: row.get(3)?,
                        service_name: row.get(4)?,
                        service_state: row.get(5)?,
                        rootfs_state: row.get(6)?,
                        alias: row.get(7)?,
                        session_id: row.get(8)?,
                        last_exit_code: row.get(9)?,
                        stdout_log: row.get(10)?,
                        stderr_log: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub(crate) fn begin_prepare(
        &mut self,
        request_id: &str,
        source: &str,
        manifest: &crate::manifest::Manifest,
        alias: &str,
        stdout_log: &Path,
        stderr_log: &Path,
    ) -> Result<(), Error> {
        let transaction = self.connection.transaction()?;
        let existing: Option<String> = transaction
            .query_row("SELECT name FROM stacks LIMIT 1", [], |row| row.get(0))
            .optional()?;
        if let Some(existing) = existing {
            return Err(Error::Conflict(format!(
                "the vertical slice already manages stack {existing:?}"
            )));
        }
        let now = unix_time()?;
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, created_at, updated_at
             ) VALUES (?1, ?2, 'up', 'intent', ?3, ?3)",
            params![request_id, manifest.name, now],
        )?;
        transaction.execute(
            "INSERT INTO stacks(
                 name, desired_state, observed_state, manifest, revision, created_at, updated_at
             ) VALUES (?1, 'running', 'starting', ?2, ?3, ?4, ?4)",
            params![manifest.name, source, request_id, now],
        )?;
        transaction.execute(
            "INSERT INTO services(
                 stack_name, name, image, command_json, alias, observed_state, rootfs_state,
                 stdout_log_path, stderr_log_path
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'preparing', 'preparing', ?6, ?7)",
            params![
                manifest.name,
                manifest.service.name,
                manifest.service.image,
                serde_json::to_string(&manifest.service.command)
                    .expect("manifest command is serializable"),
                alias,
                stdout_log.to_string_lossy(),
                stderr_log.to_string_lossy()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn set_operation_phase(&self, request_id: &str, phase: &str) -> Result<(), Error> {
        let changed = self.connection.execute(
            "UPDATE operations SET phase = ?2, updated_at = ?3 WHERE request_id = ?1",
            params![request_id, phase, unix_time()?],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(Error::NotFound(format!("operation {request_id}")))
        }
    }

    pub(crate) fn mark_installed(&mut self, request_id: &str, stack: &str) -> Result<(), Error> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE services SET observed_state = 'starting', rootfs_state = 'installed'
              WHERE stack_name = ?1",
            [stack],
        )?;
        transaction.execute(
            "UPDATE operations SET phase = 'installed', updated_at = ?2 WHERE request_id = ?1",
            params![request_id, unix_time()?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_rootfs_unknown(&self, stack: &str) -> Result<(), Error> {
        self.connection.execute(
            "UPDATE services SET rootfs_state = 'unknown' WHERE stack_name = ?1",
            [stack],
        )?;
        Ok(())
    }

    pub(crate) fn mark_starting(
        &mut self,
        request_id: &str,
        stack: &str,
        child_pid: u32,
        child_starttime: u64,
        boot_id: &str,
    ) -> Result<(), Error> {
        let child_starttime = i64::try_from(child_starttime)
            .map_err(|_| Error::Io(io::Error::other("process starttime exceeds SQLite INTEGER")))?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE services
                SET observed_state = 'starting', child_pid = ?2,
                    child_starttime = ?3, boot_id = ?4
              WHERE stack_name = ?1",
            params![stack, child_pid, child_starttime, boot_id],
        )?;
        transaction.execute(
            "UPDATE operations SET phase = 'started', updated_at = ?2 WHERE request_id = ?1",
            params![request_id, unix_time()?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_running(
        &mut self,
        request_id: &str,
        stack: &str,
        session_id: u32,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE services SET observed_state = 'running', session_id = ?2
              WHERE stack_name = ?1",
            params![stack, session_id],
        )?;
        transaction.execute(
            "UPDATE stacks SET observed_state = 'running', updated_at = ?2 WHERE name = ?1",
            params![stack, now],
        )?;
        transaction.execute(
            "UPDATE operations
                SET phase = 'committed', outcome = 'success', updated_at = ?2
              WHERE request_id = ?1",
            params![request_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_failed(
        &mut self,
        request_id: &str,
        stack: &str,
        code: &str,
        message: &str,
        unknown: bool,
    ) -> Result<(), Error> {
        let state = if unknown { "unknown" } else { "failed" };
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE services SET observed_state = ?2 WHERE stack_name = ?1",
            params![stack, state],
        )?;
        transaction.execute(
            "UPDATE stacks SET observed_state = ?2, updated_at = ?3 WHERE name = ?1",
            params![stack, state, now],
        )?;
        transaction.execute(
            "UPDATE operations
                SET outcome = 'failure', error_code = ?2, error_message = ?3, updated_at = ?4
              WHERE request_id = ?1",
            params![request_id, code, message, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn begin_down(&mut self, request_id: &str, stack: &str) -> Result<(), Error> {
        if self.stack_status(stack)?.is_none() {
            return Err(Error::NotFound(stack.to_owned()));
        }
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, created_at, updated_at
             ) VALUES (?1, ?2, 'down', 'stop_requested', ?3, ?3)",
            params![request_id, stack, now],
        )?;
        transaction.execute(
            "UPDATE stacks SET desired_state = 'stopped', observed_state = 'stopping',
                    updated_at = ?2 WHERE name = ?1",
            params![stack, now],
        )?;
        transaction.execute(
            "UPDATE services SET observed_state = 'stopping' WHERE stack_name = ?1",
            [stack],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_stopped(
        &mut self,
        request_id: &str,
        stack: &str,
        exit_code: Option<i32>,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE services
                SET observed_state = 'stopped', session_id = NULL, child_pid = NULL,
                    child_starttime = NULL, boot_id = NULL, last_exit_code = ?2
              WHERE stack_name = ?1",
            params![stack, exit_code],
        )?;
        transaction.execute(
            "UPDATE stacks SET observed_state = 'stopped', updated_at = ?2 WHERE name = ?1",
            params![stack, now],
        )?;
        transaction.execute(
            "UPDATE operations SET phase = 'stopped', outcome = 'success', updated_at = ?2
              WHERE request_id = ?1",
            params![request_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn service_identity(&self, stack: &str) -> Result<ServiceIdentity, Error> {
        self.connection
            .query_row(
                "SELECT alias, session_id, child_pid, child_starttime, boot_id
                   FROM services WHERE stack_name = ?1",
                [stack],
                |row| {
                    Ok(ServiceIdentity {
                        alias: row.get(0)?,
                        session_id: row.get(1)?,
                        child_pid: row.get(2)?,
                        child_starttime: row.get(3)?,
                        boot_id: row.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(stack.to_owned()))
    }

    pub(crate) fn reconcile_cold_start(&self) -> Result<usize, Error> {
        let now = unix_time()?;
        let transaction = self.connection.unchecked_transaction()?;
        let mut changed = transaction.execute(
            "UPDATE services
                SET observed_state = 'failed', rootfs_state = 'absent'
              WHERE stack_name IN (
                    SELECT stack_name FROM operations
                     WHERE outcome IS NULL AND operation = 'up'
                       AND phase IN ('intent', 'logs_prepared')
              )",
            [],
        )?;
        transaction.execute(
            "UPDATE stacks SET observed_state = 'failed', updated_at = ?1
              WHERE name IN (
                    SELECT stack_name FROM operations
                     WHERE outcome IS NULL AND operation = 'up'
                       AND phase IN ('intent', 'logs_prepared')
              )",
            [now],
        )?;
        transaction.execute(
            "UPDATE operations
                SET outcome = 'failure', error_code = 'interrupted_before_engine',
                    error_message = 'daemon stopped before the engine invocation was durable',
                    updated_at = ?1
              WHERE outcome IS NULL AND operation = 'up'
                AND phase IN ('intent', 'logs_prepared')",
            [now],
        )?;

        changed += transaction.execute(
            "UPDATE services
                SET observed_state = 'failed', rootfs_state = 'installed'
              WHERE stack_name IN (
                    SELECT stack_name FROM operations
                     WHERE outcome IS NULL AND operation = 'up' AND phase = 'installed'
              )",
            [],
        )?;
        transaction.execute(
            "UPDATE stacks SET observed_state = 'failed', updated_at = ?1
              WHERE name IN (
                    SELECT stack_name FROM operations
                     WHERE outcome IS NULL AND operation = 'up' AND phase = 'installed'
              )",
            [now],
        )?;
        transaction.execute(
            "UPDATE operations
                SET outcome = 'failure', error_code = 'interrupted_after_install',
                    error_message = 'rootfs installation completed but start was not invoked',
                    updated_at = ?1
              WHERE outcome IS NULL AND operation = 'up' AND phase = 'installed'",
            [now],
        )?;

        changed += transaction.execute(
            "UPDATE services
                SET observed_state = 'unknown',
                    rootfs_state = CASE
                        WHEN rootfs_state = 'preparing' THEN 'unknown'
                        ELSE rootfs_state
                    END
              WHERE observed_state IN ('preparing', 'starting', 'running', 'stopping')",
            [],
        )?;
        if changed > 0 {
            transaction.execute(
                "UPDATE stacks SET observed_state = 'unknown', updated_at = ?1
                  WHERE name IN (
                        SELECT stack_name FROM services WHERE observed_state = 'unknown'
                  )",
                [now],
            )?;
        }
        transaction.execute(
            "UPDATE operations
                SET outcome = 'failure', error_code = 'cold_start_unknown',
                    error_message = 'daemon lost the live child handle; no effect was retried',
                    updated_at = ?1
              WHERE outcome IS NULL",
            [now],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub(crate) fn record_exit(&self, stack: &str, exit_code: Option<i32>) -> Result<(), Error> {
        let desired: String = self.connection.query_row(
            "SELECT desired_state FROM stacks WHERE name = ?1",
            [stack],
            |row| row.get(0),
        )?;
        let state = if desired == "stopped" {
            "stopped"
        } else {
            "failed"
        };
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE services
                SET observed_state = ?2, session_id = NULL, child_pid = NULL,
                    child_starttime = NULL, boot_id = NULL, last_exit_code = ?3
              WHERE stack_name = ?1",
            params![stack, state, exit_code],
        )?;
        transaction.execute(
            "UPDATE stacks SET observed_state = ?2, updated_at = ?3 WHERE name = ?1",
            params![stack, state, unix_time()?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn operation_replay(
        &self,
        request_id: &str,
    ) -> Result<Option<OperationReplay>, Error> {
        self.connection
            .query_row(
                "SELECT operation, stack_name, response_json
                   FROM operations WHERE request_id = ?1",
                [request_id],
                |row| {
                    Ok(OperationReplay {
                        operation: row.get(0)?,
                        stack_name: row.get(1)?,
                        response_json: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub(crate) fn existing_stack(&self, name: &str) -> Result<Option<ExistingStack>, Error> {
        self.connection
            .query_row(
                "SELECT s.manifest, v.name, v.alias, v.observed_state, v.rootfs_state,
                        v.stdout_log_path, v.stderr_log_path
                   FROM stacks AS s
                   JOIN services AS v ON v.stack_name = s.name
                  WHERE s.name = ?1",
                [name],
                |row| {
                    Ok(ExistingStack {
                        source: row.get(0)?,
                        service_name: row.get(1)?,
                        alias: row.get(2)?,
                        service_state: row.get(3)?,
                        rootfs_state: row.get(4)?,
                        stdout_log: PathBuf::from(row.get::<_, String>(5)?),
                        stderr_log: PathBuf::from(row.get::<_, String>(6)?),
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub(crate) fn begin_reuse(&mut self, request_id: &str, stack: &str) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, created_at, updated_at
             ) VALUES (?1, ?2, 'up', 'rootfs_reused', ?3, ?3)",
            params![request_id, stack, now],
        )?;
        transaction.execute(
            "UPDATE stacks SET desired_state = 'running', observed_state = 'starting',
                    revision = ?2, updated_at = ?3 WHERE name = ?1",
            params![stack, request_id, now],
        )?;
        transaction.execute(
            "UPDATE services
                SET observed_state = 'starting', session_id = NULL, child_pid = NULL,
                    child_starttime = NULL, boot_id = NULL, last_exit_code = NULL
              WHERE stack_name = ?1",
            [stack],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn begin_retry_prepare(
        &mut self,
        request_id: &str,
        stack: &str,
        alias: &str,
    ) -> Result<(), Error> {
        let now = unix_time()?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, created_at, updated_at
             ) VALUES (?1, ?2, 'up', 'intent', ?3, ?3)",
            params![request_id, stack, now],
        )?;
        transaction.execute(
            "UPDATE stacks SET desired_state = 'running', observed_state = 'starting',
                    revision = ?2, updated_at = ?3 WHERE name = ?1",
            params![stack, request_id, now],
        )?;
        transaction.execute(
            "UPDATE services
                SET alias = ?2, observed_state = 'preparing', rootfs_state = 'preparing',
                    session_id = NULL, child_pid = NULL, child_starttime = NULL,
                    boot_id = NULL, last_exit_code = NULL
              WHERE stack_name = ?1",
            params![stack, alias],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn record_noop_up(&self, request_id: &str, stack: &str) -> Result<(), Error> {
        let now = unix_time()?;
        self.connection.execute(
            "INSERT INTO operations(
                 request_id, stack_name, operation, phase, outcome, created_at, updated_at
             ) VALUES (?1, ?2, 'up', 'already_running', 'success', ?3, ?3)",
            params![request_id, stack, now],
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
         ) STRICT;
         CREATE TABLE stacks (
             name TEXT PRIMARY KEY,
             desired_state TEXT NOT NULL,
             observed_state TEXT NOT NULL,
             manifest TEXT NOT NULL,
             revision TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE services (
             stack_name TEXT NOT NULL REFERENCES stacks(name) ON DELETE CASCADE,
             name TEXT NOT NULL,
             image TEXT NOT NULL,
             command_json TEXT NOT NULL,
             alias TEXT NOT NULL UNIQUE,
             observed_state TEXT NOT NULL,
             rootfs_state TEXT NOT NULL,
             session_id INTEGER,
             child_pid INTEGER,
             child_starttime INTEGER,
             boot_id TEXT,
             last_exit_code INTEGER,
             stdout_log_path TEXT NOT NULL,
             stderr_log_path TEXT NOT NULL,
             PRIMARY KEY (stack_name, name)
         ) STRICT;
         CREATE TABLE operations (
             request_id TEXT PRIMARY KEY,
             stack_name TEXT NOT NULL,
             operation TEXT NOT NULL,
             phase TEXT NOT NULL,
             outcome TEXT,
             error_code TEXT,
             error_message TEXT,
             response_json TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         ) STRICT;",
    )?;
    transaction.execute(
        "INSERT INTO meta(key, value) VALUES ('installation_id', ?1)",
        [installation_id],
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
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
    use super::{Error, Store};
    use crate::paths::RuntimePaths;
    use std::fs;

    #[test]
    fn initializes_and_reopens_the_database() {
        let prefix = crate::paths::test_prefix("store");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");

        let first = Store::open(&paths.database_path()).expect("open store");
        let installation_id = first.installation_id().to_owned();
        assert_eq!(installation_id.len(), 32);
        assert!(first.stack_status("missing").expect("status").is_none());
        drop(first);

        let second = Store::open(&paths.database_path()).expect("reopen store");
        assert_eq!(second.installation_id(), installation_id);
        drop(second);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn journals_the_vertical_lifecycle() {
        let prefix = crate::paths::test_prefix("store-lifecycle");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let mut store = Store::open(&paths.database_path()).expect("open store");
        let source = "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: hello\nservices:\n  app:\n    image: alpine:3.22\n";
        let manifest = crate::manifest::parse(source).expect("parse manifest");

        store
            .begin_prepare(
                "up-1",
                source,
                &manifest,
                "txs-install-hello-app-random",
                &prefix.join("stdout.log"),
                &prefix.join("stderr.log"),
            )
            .expect("prepare intent");
        store
            .set_operation_phase("up-1", "install_invoked")
            .expect("install intent");
        store.mark_installed("up-1", "hello").expect("installed");
        store
            .mark_starting("up-1", "hello", 123, 456, "boot")
            .expect("starting");
        store.mark_running("up-1", "hello", 123).expect("running");
        let response = crate::protocol::Response::success(
            "up-1",
            serde_json::json!({"observed_state": "running"}),
        );
        store
            .cache_response("up-1", &response)
            .expect("cache response");
        let replay = store
            .operation_replay("up-1")
            .expect("load replay")
            .expect("operation");
        assert_eq!(replay.operation, "up");
        assert_eq!(replay.stack_name, "hello");
        assert!(
            replay
                .response_json
                .expect("cached response")
                .contains("running")
        );
        let running = store.stack_status("hello").expect("status").expect("stack");
        assert_eq!(running.desired_state, "running");
        assert_eq!(running.observed_state, "running");
        assert_eq!(running.rootfs_state, "installed");
        assert_eq!(running.session_id, Some(123));

        store.begin_down("down-1", "hello").expect("down intent");
        store
            .mark_stopped("down-1", "hello", Some(0))
            .expect("stopped");
        let stopped = store.stack_status("hello").expect("status").expect("stack");
        assert_eq!(stopped.desired_state, "stopped");
        assert_eq!(stopped.observed_state, "stopped");
        assert_eq!(stopped.rootfs_state, "installed");
        assert_eq!(stopped.last_exit_code, Some(0));

        drop(store);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn preserves_an_unknown_schema_version() {
        let prefix = crate::paths::test_prefix("store-schema");
        let paths = RuntimePaths::new(prefix.clone());
        paths.prepare().expect("prepare paths");
        let store = Store::open(&paths.database_path()).expect("open store");
        drop(store);
        let connection = rusqlite::Connection::open(paths.database_path()).expect("open SQLite");
        connection
            .pragma_update(None, "user_version", 99)
            .expect("set future schema");
        drop(connection);

        assert!(matches!(
            Store::open(&paths.database_path()),
            Err(Error::Schema(99))
        ));
        let connection = rusqlite::Connection::open(paths.database_path()).expect("inspect SQLite");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read schema");
        assert_eq!(version, 99);
        drop(connection);
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn cold_reconciliation_distinguishes_durable_phases() {
        for (phase, expected_service, expected_rootfs) in [
            ("intent", "failed", "absent"),
            ("installed", "failed", "installed"),
            ("start_invoked", "unknown", "installed"),
        ] {
            let prefix = crate::paths::test_prefix(&format!("reconcile-{phase}"));
            let paths = RuntimePaths::new(prefix.clone());
            paths.prepare().expect("prepare paths");
            let mut store = Store::open(&paths.database_path()).expect("open store");
            let source = "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: hello\nservices:\n  app:\n    image: alpine:3.22\n";
            let manifest = crate::manifest::parse(source).expect("parse manifest");
            store
                .begin_prepare(
                    "up-1",
                    source,
                    &manifest,
                    "txs-install-hello-app-random",
                    &prefix.join("stdout.log"),
                    &prefix.join("stderr.log"),
                )
                .expect("prepare intent");
            if matches!(phase, "installed" | "start_invoked") {
                store.mark_installed("up-1", "hello").expect("installed");
            }
            if phase == "start_invoked" {
                store
                    .set_operation_phase("up-1", "start_invoked")
                    .expect("start intent");
            }

            assert_eq!(store.reconcile_cold_start().expect("reconcile"), 1);
            let status = store.stack_status("hello").expect("status").expect("stack");
            assert_eq!(status.service_state, expected_service, "phase={phase}");
            assert_eq!(status.rootfs_state, expected_rootfs, "phase={phase}");
            let outcome: String = store
                .connection
                .query_row(
                    "SELECT outcome FROM operations WHERE request_id = 'up-1'",
                    [],
                    |row| row.get(0),
                )
                .expect("operation outcome");
            assert_eq!(outcome, "failure");
            drop(store);
            fs::remove_dir_all(prefix).expect("remove test prefix");
        }
    }
}
