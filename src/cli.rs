use std::ffi::{OsStr, OsString};
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
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
  termux-stacks daemon

Commands:
  config validate FILE  Validate the supported manifest profile locally
  up FILE               Reconcile the stack described by FILE
  status STACK           Show the persisted and observed stack state
  down STACK             Stop and remove the stack runtime
  daemon                 Run the foreground control-plane process

The S5 vertical slice supports one stack with one service.
";

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Help,
    Version,
    ConfigValidate(PathBuf),
    Up(PathBuf),
    Status(String),
    Down(String),
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
                println!(
                    "valid stack {:?}: service {:?}, image {:?}",
                    manifest.name, manifest.service.name, manifest.service.image
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
            send_control(
                "up",
                crate::protocol::Request::Up {
                    protocol_version: crate::protocol::VERSION,
                    request_id: request_id(),
                    manifest: source,
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
    use super::{Command, parse};
    use std::ffi::OsString;

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
