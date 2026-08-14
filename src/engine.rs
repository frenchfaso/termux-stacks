use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

const SUPPORTED_VERSION: &str = "5.6.0";

#[derive(Clone, Debug)]
pub(crate) struct Engine {
    binary: PathBuf,
    architecture: &'static str,
}

#[derive(Debug)]
pub(crate) enum Error {
    UnsupportedHost(&'static str),
    Spawn {
        operation: &'static str,
        source: io::Error,
    },
    Failed {
        operation: &'static str,
        status: ExitStatus,
        stderr: String,
    },
    Capability(String),
    InvalidSessionOutput(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHost(architecture) => {
                write!(formatter, "unsupported host architecture {architecture:?}")
            }
            Self::Spawn { operation, source } => {
                write!(formatter, "cannot start proot-distro {operation}: {source}")
            }
            Self::Failed {
                operation,
                status,
                stderr,
            } => write!(
                formatter,
                "proot-distro {operation} failed with {status}: {}",
                stderr.trim()
            ),
            Self::Capability(message) => formatter.write_str(message),
            Self::InvalidSessionOutput(line) => {
                write!(formatter, "invalid proot-distro ps --quiet line {line:?}")
            }
        }
    }
}

impl Engine {
    pub(crate) fn discover() -> Result<Self, Error> {
        let architecture = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            "arm" => "arm",
            "x86_64" => "x86_64",
            "x86" => "i686",
            other => return Err(Error::UnsupportedHost(other)),
        };
        Ok(Self {
            binary: PathBuf::from("proot-distro"),
            architecture,
        })
    }

    #[cfg(test)]
    fn with_binary(binary: PathBuf) -> Self {
        Self {
            binary,
            architecture: "aarch64",
        }
    }

    pub(crate) fn probe(&self) -> Result<(), Error> {
        let output = self.output("capability probe", ["help"])?;
        if !output.status.success() {
            return Err(failed("capability probe", output));
        }
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let has_version = combined
            .split(|character: char| !character.is_ascii_digit() && character != '.')
            .any(|token| token == SUPPORTED_VERSION);
        if !has_version {
            return Err(Error::Capability(format!(
                "proot-distro capability probe does not confirm version {SUPPORTED_VERSION}"
            )));
        }
        for required in ["install", "run", "ps", "kill", "remove"] {
            if !combined.contains(required) {
                return Err(Error::Capability(format!(
                    "proot-distro capability probe does not confirm {required:?}"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn install(&self, alias: &str, image: &str) -> Result<(), Error> {
        let output = self.output(
            "install",
            [
                "install",
                "--architecture",
                self.architecture,
                "--name",
                alias,
                image,
            ],
        )?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failed("install", output))
        }
    }

    pub(crate) fn run(
        &self,
        alias: &str,
        command: Option<&[String]>,
        stdout: File,
        stderr: File,
    ) -> Result<Child, Error> {
        let mut process = self.command();
        process.args(["run", "--isolated", alias]);
        if let Some(command) = command {
            process.arg("--").args(command);
        }
        process
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|source| Error::Spawn {
                operation: "run",
                source,
            })
    }

    pub(crate) fn sessions(&self) -> Result<Vec<u32>, Error> {
        let output = self.output("ps --quiet", ["ps", "--quiet"])?;
        if !output.status.success() {
            return Err(failed("ps --quiet", output));
        }
        parse_session_output(&output.stdout)
    }

    pub(crate) fn kill(&self, session: u32) -> Result<(), Error> {
        let session = session.to_string();
        let output = self.output("kill", ["kill", session.as_str()])?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failed("kill", output))
        }
    }

    fn output<const N: usize>(
        &self,
        operation: &'static str,
        arguments: [&str; N],
    ) -> Result<Output, Error> {
        self.command()
            .args(arguments)
            .output()
            .map_err(|source| Error::Spawn { operation, source })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("PD_FORCE_NO_COLORS", "true")
            .env("COLUMNS", "240");
        command
    }
}

fn parse_session_output(bytes: &[u8]) -> Result<Vec<u32>, Error> {
    let stdout = std::str::from_utf8(bytes)
        .map_err(|_| Error::InvalidSessionOutput("<non-UTF-8>".into()))?;
    let mut sessions = Vec::new();
    let mut unique = BTreeSet::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if !line.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InvalidSessionOutput(line.to_owned()));
        }
        let session = line
            .parse::<u32>()
            .map_err(|_| Error::InvalidSessionOutput(line.to_owned()))?;
        if session == 0 || !unique.insert(session) {
            return Err(Error::InvalidSessionOutput(line.to_owned()));
        }
        sessions.push(session);
    }
    Ok(sessions)
}

fn failed(operation: &'static str, output: Output) -> Error {
    Error::Failed {
        operation,
        status: output.status,
        stderr: bounded_text(&output.stderr),
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    const MAX: usize = 16 * 1024;
    let end = bytes.len().min(MAX);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if bytes.len() > MAX {
        text.push_str("\n[diagnostic truncated]");
    }
    text
}

pub(crate) fn process_starttime(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end = stat
        .rfind(") ")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid /proc stat"))?;
    let fields: Vec<&str> = stat[end + 2..].split_ascii_whitespace().collect();
    fields
        .get(19)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short /proc stat"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid process starttime"))
}

pub(crate) fn boot_id() -> io::Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::{Engine, bounded_text, parse_session_output};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn capability_probe_requires_the_version_and_commands() {
        let prefix = crate::paths::test_prefix("engine-probe");
        let binary = prefix.join("proot-distro");
        let shell = std::env::var_os("PREFIX")
            .map(|prefix| std::path::PathBuf::from(prefix).join("bin/sh"))
            .unwrap_or_else(|| "/bin/sh".into());
        fs::write(
            &binary,
            format!(
                "#!{}\nprintf '%s\\n' 'install run ps kill remove proot-distro 5.6.0'\n",
                shell.display()
            ),
        )
        .expect("write fake engine");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make fake executable");
        Engine::with_binary(binary)
            .probe()
            .expect("capability probe");
        fs::remove_dir_all(prefix).expect("remove test prefix");
    }

    #[test]
    fn diagnostics_are_bounded() {
        let text = bounded_text(&vec![b'x'; 20 * 1024]);
        assert!(text.len() < 17 * 1024);
        assert!(text.ends_with("[diagnostic truncated]"));
    }

    #[test]
    fn parses_only_the_qualified_quiet_session_grammar() {
        assert_eq!(
            parse_session_output(b"123\n456\n").expect("valid sessions"),
            [123, 456]
        );
        assert!(
            parse_session_output(b"")
                .expect("empty is valid")
                .is_empty()
        );
        for invalid in [b" 123\n".as_slice(), b"0\n", b"123\n123\n", b"pid=123\n"] {
            assert!(parse_session_output(invalid).is_err(), "{invalid:?}");
        }
    }
}
