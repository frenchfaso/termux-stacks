use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

const HELP: &str = "Termux Stacks

Usage:
  termux-stacks --help
  termux-stacks --version
  termux-stacks daemon

Commands:
  daemon       Run the foreground control-plane process

The public lifecycle commands are not implemented yet.
";

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Help,
    Version,
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
    fn rejects_unknown_commands() {
        let error = parse(args(&["up"])).expect_err("up is not implemented");
        assert!(error.contains("unknown command"));
    }

    #[test]
    fn rejects_extra_arguments() {
        let error = parse(args(&["daemon", "extra"])).expect_err("extra arg");
        assert!(error.contains("unexpected argument"));
    }
}
