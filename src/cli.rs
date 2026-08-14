use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "Termux Stacks

Usage:
  termux-stacks --help
  termux-stacks --version
  termux-stacks config validate FILE
  termux-stacks up FILE
  termux-stacks status STACK
  termux-stacks down STACK
  termux-stacks logs STACK SERVICE [--tail N]
  termux-stacks restart STACK SERVICE
  termux-stacks daemon

Commands:
  config validate FILE  Validate the supported manifest profile locally
  up FILE               Reconcile the stack described by FILE
  status STACK           Show the persisted and observed stack state
  down STACK             Stop the stack; retain rootfs, logs, and volumes
  logs STACK SERVICE     Show up to 200 lines from the service logs
  restart STACK SERVICE  Restart a service on its current rootfs
  daemon                 Run the foreground control-plane process

The M1 MVP supports multiple stacks and services within Termux's limits.
";

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Help,
    Version,
    ConfigValidate(PathBuf),
    Up(PathBuf),
    Status(String),
    Down(String),
    Logs {
        stack: String,
        service: String,
        tail: u16,
    },
    Restart {
        stack: String,
        service: String,
    },
    Daemon,
}

pub(crate) fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    match parse(args) {
        Ok(Command::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("termux-stacks {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::ConfigValidate(path)) => match crate::manifest::load(&path) {
            Ok((manifest, _source)) => {
                let manifest_base = match canonical_manifest_base(&path) {
                    Ok(base) => base,
                    Err(error) => {
                        eprintln!("termux-stacks config validate: [io] {error}");
                        return ExitCode::FAILURE;
                    }
                };
                if let Err(error) =
                    crate::resources::validate_bind_sources(&manifest_base, &manifest)
                {
                    eprintln!("termux-stacks config validate: [invalid_resource] {error}");
                    return ExitCode::FAILURE;
                }
                let services = manifest.services.keys().cloned().collect::<Vec<_>>();
                println!(
                    "valid stack {:?}: {} service(s) {:?}",
                    manifest.name,
                    services.len(),
                    services
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!(
                    "termux-stacks config validate: [{}] {error}",
                    error.kind().code()
                );
                ExitCode::FAILURE
            }
        },
        Ok(Command::Up(path)) => {
            let source = match crate::manifest::load(&path) {
                Ok((_manifest, source)) => source,
                Err(error) => {
                    eprintln!("termux-stacks up: [{}] {error}", error.kind().code());
                    return ExitCode::FAILURE;
                }
            };
            let manifest_base = match canonical_manifest_base(&path) {
                Ok(base) => base,
                Err(error) => {
                    eprintln!("termux-stacks up: [io] {error}");
                    return ExitCode::FAILURE;
                }
            };
            send_control(
                "up",
                crate::protocol::Request::Up {
                    protocol_version: crate::protocol::VERSION,
                    request_id: request_id(),
                    manifest: source,
                    manifest_base,
                },
            )
        }
        Ok(Command::Status(stack)) => send_named_request("status", stack, |request_id, stack| {
            crate::protocol::Request::Status {
                protocol_version: crate::protocol::VERSION,
                request_id,
                stack,
            }
        }),
        Ok(Command::Down(stack)) => send_named_request("down", stack, |request_id, stack| {
            crate::protocol::Request::Down {
                protocol_version: crate::protocol::VERSION,
                request_id,
                stack,
            }
        }),
        Ok(Command::Logs {
            stack,
            service,
            tail,
        }) => send_service_request("logs", stack, service, |request_id, stack, service| {
            crate::protocol::Request::Logs {
                protocol_version: crate::protocol::VERSION,
                request_id,
                stack,
                service,
                tail,
            }
        }),
        Ok(Command::Restart { stack, service }) => {
            send_service_request("restart", stack, service, |request_id, stack, service| {
                crate::protocol::Request::Restart {
                    protocol_version: crate::protocol::VERSION,
                    request_id,
                    stack,
                    service,
                }
            })
        }
        Ok(Command::Daemon) => match crate::daemon::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("termux-stacks daemon: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("termux-stacks: {error}");
            eprintln!("Try 'termux-stacks --help' for usage.");
            ExitCode::from(2)
        }
    }
}

fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Command::Help);
    };

    if first == OsStr::new("-h") || first == OsStr::new("--help") {
        return no_extra_args(args, Command::Help);
    }
    if first == OsStr::new("-V") || first == OsStr::new("--version") {
        return no_extra_args(args, Command::Version);
    }
    if first == OsStr::new("config") {
        let Some(action) = args.next() else {
            return Err("missing config action; expected \"validate\"".into());
        };
        if action != OsStr::new("validate") {
            return Err(format!("unknown config action {}", quote(&action)));
        }
        let Some(path) = args.next() else {
            return Err("missing manifest path for config validate".into());
        };
        return no_extra_args(args, Command::ConfigValidate(PathBuf::from(path)));
    }
    if first == OsStr::new("up") {
        return one_path_arg(args, "up", Command::Up);
    }
    if first == OsStr::new("status") {
        return one_string_arg(args, "status", Command::Status);
    }
    if first == OsStr::new("down") {
        return one_string_arg(args, "down", Command::Down);
    }
    if first == OsStr::new("logs") {
        return parse_logs(args);
    }
    if first == OsStr::new("restart") {
        return parse_service_command(args, "restart", |stack, service| Command::Restart {
            stack,
            service,
        });
    }
    if first == OsStr::new("daemon") {
        if let Some(second) = args.next() {
            if (second == OsStr::new("-h") || second == OsStr::new("--help"))
                && args.next().is_none()
            {
                return Ok(Command::Help);
            }
            return Err(format!("unexpected argument {}", quote(&second)));
        }
        return Ok(Command::Daemon);
    }

    Err(format!("unknown command or option {}", quote(&first)))
}

fn parse_logs<I>(mut args: I) -> Result<Command, String>
where
    I: Iterator<Item = OsString>,
{
    let stack = next_utf8(&mut args, "missing stack argument for logs")?;
    let service = next_utf8(&mut args, "missing service argument for logs")?;
    let Some(option) = args.next() else {
        return Ok(Command::Logs {
            stack,
            service,
            tail: crate::protocol::DEFAULT_LOG_TAIL,
        });
    };
    if option != OsStr::new("--tail") {
        return Err(format!("unexpected argument {}", quote(&option)));
    }
    let raw_tail = args
        .next()
        .ok_or_else(|| "missing value for logs --tail".to_owned())?;
    let raw_tail = raw_tail
        .into_string()
        .map_err(|value| format!("logs --tail must be UTF-8, got {}", quote(&value)))?;
    let tail = parse_log_tail(&raw_tail)?;
    no_extra_args(
        args,
        Command::Logs {
            stack,
            service,
            tail,
        },
    )
}

fn parse_service_command<I, F>(mut args: I, command_name: &str, build: F) -> Result<Command, String>
where
    I: Iterator<Item = OsString>,
    F: FnOnce(String, String) -> Command,
{
    let stack = next_utf8(
        &mut args,
        &format!("missing stack argument for {command_name}"),
    )?;
    let service = next_utf8(
        &mut args,
        &format!("missing service argument for {command_name}"),
    )?;
    no_extra_args(args, build(stack, service))
}

fn next_utf8<I>(args: &mut I, missing: &str) -> Result<String, String>
where
    I: Iterator<Item = OsString>,
{
    args.next()
        .ok_or_else(|| missing.to_owned())?
        .into_string()
        .map_err(|value| format!("argument must be UTF-8, got {}", quote(&value)))
}

fn parse_log_tail(value: &str) -> Result<u16, String> {
    let invalid = || {
        format!(
            "logs --tail must be a decimal integer from 1 to {}",
            crate::protocol::MAX_LOG_TAIL
        )
    };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let tail = value.parse::<u16>().map_err(|_| invalid())?;
    if tail == 0 || tail > crate::protocol::MAX_LOG_TAIL {
        return Err(invalid());
    }
    Ok(tail)
}

fn one_path_arg<I, F>(mut args: I, command_name: &str, build: F) -> Result<Command, String>
where
    I: Iterator<Item = OsString>,
    F: FnOnce(PathBuf) -> Command,
{
    let Some(value) = args.next() else {
        return Err(format!("missing argument for {command_name}"));
    };
    no_extra_args(args, build(PathBuf::from(value)))
}

fn one_string_arg<I, F>(mut args: I, command_name: &str, build: F) -> Result<Command, String>
where
    I: Iterator<Item = OsString>,
    F: FnOnce(String) -> Command,
{
    let Some(value) = args.next() else {
        return Err(format!("missing argument for {command_name}"));
    };
    let value = value
        .into_string()
        .map_err(|value| format!("argument must be UTF-8, got {}", quote(&value)))?;
    no_extra_args(args, build(value))
}

fn send_named_request<F>(command: &str, stack: String, build: F) -> ExitCode
where
    F: FnOnce(String, String) -> crate::protocol::Request,
{
    if let Err(error) = crate::manifest::validate_stack_name(&stack) {
        eprintln!("termux-stacks {command}: [{}] {error}", error.kind().code());
        return ExitCode::FAILURE;
    }
    send_control(command, build(request_id(), stack))
}

fn send_service_request<F>(command: &str, stack: String, service: String, build: F) -> ExitCode
where
    F: FnOnce(String, String, String) -> crate::protocol::Request,
{
    if !validate_cli_name(command, "stack", &stack)
        || !validate_cli_name(command, "service", &service)
    {
        return ExitCode::FAILURE;
    }
    send_control(command, build(request_id(), stack, service))
}

fn validate_cli_name(command: &str, kind: &str, value: &str) -> bool {
    if let Err(error) = crate::manifest::validate_stack_name(value) {
        eprintln!(
            "termux-stacks {command}: [{}] invalid {kind} name {value:?}: names must match ^[a-z][a-z0-9-]{{0,47}}$ and must not start with termux-stacks-",
            error.kind().code()
        );
        false
    } else {
        true
    }
}

fn canonical_manifest_base(path: &Path) -> Result<String, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical = fs::canonicalize(parent).map_err(|error| {
        format!(
            "cannot resolve manifest directory {}: {error}",
            parent.display()
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        format!(
            "cannot inspect manifest directory {}: {error}",
            canonical.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "manifest base is not a directory: {}",
            canonical.display()
        ));
    }
    canonical
        .into_os_string()
        .into_string()
        .map_err(|value| format!("manifest directory must be UTF-8, got {}", quote(&value)))
}

fn send_control(command: &str, request: crate::protocol::Request) -> ExitCode {
    let Some(prefix) = std::env::var_os("PREFIX") else {
        eprintln!("termux-stacks {command}: PREFIX is not set");
        return ExitCode::FAILURE;
    };
    let prefix = PathBuf::from(prefix);
    if !prefix.is_absolute() {
        eprintln!("termux-stacks {command}: PREFIX must be absolute");
        return ExitCode::FAILURE;
    }
    let paths = crate::paths::RuntimePaths::new(prefix);
    let mut stream = match UnixStream::connect(paths.socket_path()) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("termux-stacks {command}: cannot connect to daemon: {error}");
            return ExitCode::FAILURE;
        }
    };
    let expected_id = request.request_id().to_owned();
    if let Err(error) = crate::protocol::write_frame(&mut stream, &request) {
        eprintln!("termux-stacks {command}: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = stream.shutdown(Shutdown::Write) {
        eprintln!("termux-stacks {command}: cannot finish request: {error}");
        return ExitCode::FAILURE;
    }

    let response = match crate::protocol::read_response(&mut BufReader::new(stream)) {
        Ok(Some(response)) => response,
        Ok(None) => {
            eprintln!("termux-stacks {command}: daemon closed the connection without a response");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("termux-stacks {command}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = response.validate(&expected_id) {
        eprintln!("termux-stacks {command}: {error}");
        return ExitCode::FAILURE;
    }
    if response.ok {
        println!(
            "{}",
            serde_json::to_string_pretty(&response.result.expect("validated result"))
                .expect("JSON value is serializable")
        );
        ExitCode::SUCCESS
    } else {
        let error = response.error.expect("validated error");
        eprintln!(
            "termux-stacks {command}: [{}] {}",
            error.code, error.message
        );
        ExitCode::FAILURE
    }
}

fn request_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{timestamp:x}-{sequence:x}", std::process::id())
}

fn no_extra_args<I>(mut args: I, command: Command) -> Result<Command, String>
where
    I: Iterator<Item = OsString>,
{
    match args.next() {
        Some(extra) => Err(format!("unexpected argument {}", quote(&extra))),
        None => Ok(command),
    }
}

fn quote(value: &OsStr) -> String {
    format!("{:?}", value.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::{Command, canonical_manifest_base, parse, validate_cli_name};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_shows_help() {
        assert_eq!(parse(Vec::new()), Ok(Command::Help));
    }

    #[test]
    fn parses_version() {
        assert_eq!(parse(args(&["--version"])), Ok(Command::Version));
    }

    #[test]
    fn parses_daemon() {
        assert_eq!(parse(args(&["daemon"])), Ok(Command::Daemon));
    }

    #[test]
    fn parses_config_validate() {
        assert_eq!(
            parse(args(&["config", "validate", "stack.yaml"])),
            Ok(Command::ConfigValidate("stack.yaml".into()))
        );
    }

    #[test]
    fn parses_lifecycle_commands() {
        assert_eq!(
            parse(args(&["up", "stack.yaml"])),
            Ok(Command::Up("stack.yaml".into()))
        );
        assert_eq!(
            parse(args(&["status", "hello"])),
            Ok(Command::Status("hello".into()))
        );
        assert_eq!(
            parse(args(&["down", "hello"])),
            Ok(Command::Down("hello".into()))
        );
        assert_eq!(
            parse(args(&["restart", "hello", "api"])),
            Ok(Command::Restart {
                stack: "hello".into(),
                service: "api".into(),
            })
        );
    }

    #[test]
    fn parses_logs_with_a_bounded_decimal_tail() {
        assert_eq!(
            parse(args(&["logs", "hello", "api"])),
            Ok(Command::Logs {
                stack: "hello".into(),
                service: "api".into(),
                tail: 200,
            })
        );
        assert_eq!(
            parse(args(&["logs", "hello", "api", "--tail", "1"])),
            Ok(Command::Logs {
                stack: "hello".into(),
                service: "api".into(),
                tail: 1,
            })
        );
        assert_eq!(
            parse(args(&["logs", "hello", "api", "--tail", "17"])),
            Ok(Command::Logs {
                stack: "hello".into(),
                service: "api".into(),
                tail: 17,
            })
        );
    }

    #[test]
    fn rejects_invalid_log_tail_values() {
        for value in ["", "0", "+1", "-1", "1.0", "201", "65536"] {
            let error = parse(args(&["logs", "hello", "api", "--tail", value]))
                .expect_err("invalid tail must fail");
            assert!(error.contains("decimal integer from 1 to 200"), "{error:?}");
        }
        let missing =
            parse(args(&["logs", "hello", "api", "--tail"])).expect_err("tail value is required");
        assert!(missing.contains("missing value"));
    }

    #[test]
    fn service_commands_validate_both_names() {
        assert!(validate_cli_name("logs", "stack", "notes"));
        assert!(validate_cli_name("logs", "service", "api-1"));
        assert!(!validate_cli_name("logs", "stack", "Notes"));
        assert!(!validate_cli_name(
            "restart",
            "service",
            "termux-stacks-internal"
        ));
    }

    #[test]
    fn manifest_base_is_absolute_canonical_and_utf8() {
        let actual = canonical_manifest_base(Path::new("termux-stacks.yaml"))
            .expect("canonical manifest base");
        let expected = std::fs::canonicalize(".").expect("canonical current directory");
        assert_eq!(PathBuf::from(actual), expected);
    }

    #[test]
    fn rejects_incomplete_config_validate() {
        let error = parse(args(&["config", "validate"])).expect_err("path is required");
        assert!(error.contains("missing manifest path"));
    }

    #[test]
    fn rejects_unknown_commands() {
        let error = parse(args(&["unknown"])).expect_err("unknown command must fail");
        assert!(error.contains("unknown command"));
    }

    #[test]
    fn rejects_extra_arguments() {
        let error = parse(args(&["daemon", "extra"])).expect_err("extra arg");
        assert!(error.contains("unexpected argument"));
    }
}
