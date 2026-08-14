use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[test]
fn help_uses_the_real_binary() {
    let output = run(&["--help"]);

    assert!(output.status.success(), "{output:?}");
    assert!(text(&output.stdout).contains("Usage:\n  termux-stacks --help"));
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn version_uses_the_real_binary() {
    let output = run(&["--version"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        text(&output.stdout),
        format!("termux-stacks {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn unknown_command_uses_the_real_binary() {
    let output = run(&["unknown"]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(text(&output.stderr).contains("unknown command or option \"unknown\""));
}

#[test]
fn config_validate_uses_the_real_binary() {
    let prefix = TestPrefix::new("manifest");
    let manifest = prefix.path().join("termux-stacks.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: hello\nservices:\n  app:\n    image: alpine:3.22\n",
    )
    .expect("write manifest");

    let output = Command::new(binary())
        .args(["config", "validate"])
        .arg(&manifest)
        .output()
        .expect("validate manifest");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        text(&output.stdout),
        "valid stack \"hello\": service \"app\", image \"alpine:3.22\"\n"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn config_validate_reports_an_invalid_manifest() {
    let prefix = TestPrefix::new("invalid-manifest");
    let manifest = prefix.path().join("termux-stacks.yaml");
    fs::write(&manifest, "kind: Stack\n").expect("write manifest");

    let output = Command::new(binary())
        .args(["config", "validate"])
        .arg(&manifest)
        .output()
        .expect("validate manifest");

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        text(&output.stderr).contains("[invalid_manifest] missing required field apiVersion"),
        "{output:?}"
    );
}

#[test]
fn daemon_is_a_singleton_across_processes() {
    let prefix = TestPrefix::new("singleton");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut first = DaemonProcess::spawn(prefix.path(), "first");
    wait_until_ready(&mut first, &socket);

    let mut second = DaemonProcess::spawn(prefix.path(), "second");
    let second_status = wait_until_exit(&mut second);
    let second_stderr = second.stderr_text();

    assert!(!second_status.success(), "{second_status:?}");
    assert!(
        second_stderr.contains("another daemon is already running"),
        "stderr={second_stderr:?}"
    );
    assert!(first.is_running(), "first daemon exited unexpectedly");

    first.kill_and_wait();
}

#[test]
fn daemon_recovers_a_stale_socket_after_sigkill() {
    let prefix = TestPrefix::new("restart");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut first = DaemonProcess::spawn(prefix.path(), "first");
    wait_until_ready(&mut first, &socket);

    first.kill_and_wait();
    let stale = fs::symlink_metadata(&socket).expect("SIGKILL must leave the socket path behind");
    assert!(stale.file_type().is_socket());

    let mut restarted = DaemonProcess::spawn(prefix.path(), "restarted");
    wait_until_ready(&mut restarted, &socket);
    assert!(
        restarted.is_running(),
        "restarted daemon exited unexpectedly"
    );

    restarted.kill_and_wait();
}

#[test]
fn status_round_trips_through_the_daemon() {
    let prefix = TestPrefix::new("status");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let output = Command::new(binary())
        .args(["status", "hello"])
        .env("PREFIX", prefix.path())
        .output()
        .expect("request status");

    assert!(output.status.success(), "{output:?}");
    assert!(
        text(&output.stdout).contains("\"observed_state\": \"absent\""),
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    daemon.kill_and_wait();
}

#[test]
fn daemon_exits_cleanly_on_sigterm() {
    let prefix = TestPrefix::new("sigterm");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);
    daemon.wait_until_stdout_contains("daemon ready");

    let status = daemon.terminate_and_wait();
    assert!(status.success(), "{status:?}; {}", daemon.diagnostics());
    assert!(!socket.exists(), "graceful shutdown must remove the socket");
}

#[cfg(target_os = "linux")]
#[test]
fn vertical_lifecycle_uses_the_fake_engine_contract() {
    let prefix = TestPrefix::new("lifecycle");
    let manifest = prefix.path().join("termux-stacks.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: hello\nservices:\n  app:\n    image: fake:latest\n",
    )
    .expect("write manifest");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);
    daemon.wait_until_stdout_contains("daemon ready");

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    assert!(text(&up.stdout).contains("\"observed_state\": \"running\""));

    let down = run_with_prefix(prefix.path(), &["down", "hello"]);
    assert!(down.status.success(), "{down:?}");
    assert!(text(&down.stdout).contains("\"observed_state\": \"stopped\""));
    assert!(
        prefix
            .path()
            .join("var/lib/termux-stacks/logs/hello/app.stdout.log")
            .is_file()
    );

    assert!(daemon.terminate_and_wait().success());
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_termux-stacks")
}

fn run(arguments: &[&str]) -> Output {
    Command::new(binary())
        .args(arguments)
        .output()
        .expect("run termux-stacks")
}

#[cfg(target_os = "linux")]
fn run_with_prefix(prefix: &Path, arguments: &[&str]) -> Output {
    Command::new(binary())
        .args(arguments)
        .env("PREFIX", prefix)
        .output()
        .expect("run termux-stacks with PREFIX")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn wait_until_ready(process: &mut DaemonProcess, socket: &Path) {
    let deadline = Instant::now() + READY_TIMEOUT;

    loop {
        if let Some(status) = process.try_wait() {
            panic!(
                "daemon exited before its socket was ready: {status}; {}",
                process.diagnostics()
            );
        }

        let connect_error = match UnixStream::connect(socket) {
            Ok(stream) => {
                drop(stream);
                return;
            }
            Err(error) => error,
        };

        if Instant::now() >= deadline {
            panic!(
                "daemon socket {} was not ready within {READY_TIMEOUT:?}: {}; {}",
                socket.display(),
                connect_error,
                process.diagnostics()
            );
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_until_exit(process: &mut DaemonProcess) -> ExitStatus {
    let deadline = Instant::now() + READY_TIMEOUT;

    loop {
        if let Some(status) = process.try_wait() {
            return status;
        }
        if Instant::now() >= deadline {
            panic!(
                "daemon did not exit within {READY_TIMEOUT:?}; {}",
                process.diagnostics()
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

struct DaemonProcess {
    child: Option<Child>,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl DaemonProcess {
    fn spawn(prefix: &Path, label: &str) -> Self {
        let fake_bin = prefix.join("bin");
        fs::create_dir_all(&fake_bin).expect("create fake engine bin directory");
        let fake_engine = fake_bin.join("proot-distro");
        let fake_state = prefix.join("fake-engine");
        fs::create_dir_all(&fake_state).expect("create fake engine state");
        if !fake_engine.exists() {
            let shell = std::env::var_os("PREFIX")
                .map(|prefix| PathBuf::from(prefix).join("bin/sh"))
                .unwrap_or_else(|| "/bin/sh".into());
            fs::write(
                &fake_engine,
                format!(
                    "#!{}\nset -eu\nstate=${{TERMUX_STACKS_FAKE_STATE:?}}\ncase ${{1:-}} in\n  help) printf '%s\\n' 'proot-distro 5.6.0 install run ps kill remove' ;;\n  install) exit 0 ;;\n  run)\n    printf '%s\\n' $$ >\"$state/session\"\n    trap 'rm -f \"$state/session\"; exit 0' TERM INT\n    while :; do sleep 1; done\n    ;;\n  ps) [ ! -f \"$state/session\" ] || cat \"$state/session\" ;;\n  kill) kill -TERM \"$2\" ;;\n  remove) exit 0 ;;\n  *) exit 2 ;;\nesac\n",
                    shell.display()
                ),
            )
            .expect("write fake engine");
            fs::set_permissions(&fake_engine, fs::Permissions::from_mode(0o700))
                .expect("make fake engine executable");
        }
        let stdout = prefix.join(format!("{label}.stdout"));
        let stderr = prefix.join(format!("{label}.stderr"));
        let stdout_file = fs::File::create(&stdout).expect("create daemon stdout log");
        let stderr_file = fs::File::create(&stderr).expect("create daemon stderr log");
        let child = Command::new(binary())
            .arg("daemon")
            .env("PREFIX", prefix)
            .env("TERMUX_STACKS_FAKE_STATE", &fake_state)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .expect("spawn daemon");

        Self {
            child: Some(child),
            stdout,
            stderr,
        }
    }

    fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child
            .as_mut()
            .expect("daemon child already consumed")
            .try_wait()
            .expect("inspect daemon process")
    }

    fn is_running(&mut self) -> bool {
        self.try_wait().is_none()
    }

    fn kill_and_wait(&mut self) {
        let mut child = self.child.take().expect("daemon child already consumed");
        child.kill().expect("SIGKILL daemon child");
        child.wait().expect("wait for killed daemon child");
    }

    fn terminate_and_wait(&mut self) -> ExitStatus {
        let pid = self
            .child
            .as_ref()
            .expect("daemon child already consumed")
            .id();
        // SAFETY: the child PID belongs to this test process and remains owned
        // until wait_until_exit reaps it.
        let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        assert_eq!(result, 0, "send SIGTERM to daemon");
        wait_until_exit(self)
    }

    fn diagnostics(&self) -> String {
        format!(
            "stdout={:?}; stderr={:?}",
            fs::read_to_string(&self.stdout).unwrap_or_else(|error| format!("<{error}>")),
            fs::read_to_string(&self.stderr).unwrap_or_else(|error| format!("<{error}>"))
        )
    }

    fn stderr_text(&self) -> String {
        fs::read_to_string(&self.stderr).expect("read daemon stderr")
    }

    fn wait_until_stdout_contains(&mut self, expected: &str) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if fs::read_to_string(&self.stdout).is_ok_and(|text| text.contains(expected)) {
                return;
            }
            if let Some(status) = self.try_wait() {
                panic!(
                    "daemon exited before stdout contained {expected:?}: {status}; {}",
                    self.diagnostics()
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "daemon stdout did not contain {expected:?} within {READY_TIMEOUT:?}; {}",
                    self.diagnostics()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct TestPrefix {
    path: PathBuf,
}

impl TestPrefix {
    fn new(label: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);

        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .subsec_nanos();
        let name = format!("tsi-{label}-{}-{sequence}-{nanos}", std::process::id());
        let mut path = std::env::temp_dir().join(&name);

        if path
            .join("var/run/termux-stacks/daemon.sock")
            .as_os_str()
            .len()
            > 90
        {
            #[cfg(target_os = "android")]
            {
                let compact = format!("tsi-{:x}-{sequence:x}-{nanos:x}", std::process::id());
                path = std::env::temp_dir().join(compact);
            }
            #[cfg(target_os = "macos")]
            let short_temp = Path::new("/private/tmp");
            #[cfg(all(not(target_os = "macos"), not(target_os = "android")))]
            let short_temp = Path::new("/tmp");
            #[cfg(not(target_os = "android"))]
            {
                path = short_temp.join(name);
            }
        }

        fs::create_dir(&path).expect("create test PREFIX");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestPrefix {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("tsi-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
