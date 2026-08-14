use std::fs;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Write};
#[cfg(target_os = "linux")]
use std::net::TcpListener;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
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
        "valid stack \"hello\": 1 service(s) [\"app\"]\n"
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
fn config_validate_rejects_a_missing_bind_source_offline() {
    let prefix = TestPrefix::new("config-missing-bind");
    let manifest = prefix.path().join("missing-bind.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: hello\nservices:\n  app:\n    image: fake:latest\n    mounts:\n      - {type: bind, source: ./missing, target: /config}\n",
    )
    .expect("write manifest");

    let output = Command::new(binary())
        .args(["config", "validate"])
        .arg(&manifest)
        .output()
        .expect("validate manifest");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("[invalid_resource]"));
    assert!(!prefix.path().join("missing").exists());
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
    let status = output_json(&output);
    assert_eq!(status["name"], "hello");
    assert_eq!(status["observed_state"], "absent");
    assert_eq!(status["services"], serde_json::json!([]));
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
    let up_status = output_json(&up);
    assert_eq!(up_status["observed_state"], "running");
    assert_eq!(up_status["services"][0]["name"], "app");
    assert_eq!(up_status["services"][0]["observed_state"], "running");

    let down = run_with_prefix(prefix.path(), &["down", "hello"]);
    assert!(down.status.success(), "{down:?}");
    let down_status = output_json(&down);
    assert_eq!(down_status["observed_state"], "stopped");
    assert_eq!(down_status["services"][0]["observed_state"], "stopped");
    assert!(
        prefix
            .path()
            .join("var/lib/termux-stacks/logs/hello/app.stdout.log")
            .is_file()
    );

    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn two_stacks_follow_the_dag_and_support_logs_and_restart() {
    let prefix = TestPrefix::new("multi-stack");
    let alpha = write_fake_multi_manifest(prefix.path(), "alpha");
    let beta = write_fake_multi_manifest(prefix.path(), "beta");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    for manifest in [&alpha, &beta] {
        let up = run_with_prefix(
            prefix.path(),
            &["up", manifest.to_str().expect("UTF-8 path")],
        );
        assert!(up.status.success(), "{up:?}");
        let status = output_json(&up);
        assert_eq!(status["observed_state"], "running");
        let services = status["services"].as_array().expect("service array");
        assert_eq!(services.len(), 2);
        assert_eq!(services[0]["name"], "api");
        assert_eq!(services[1]["name"], "worker");
        assert!(
            services
                .iter()
                .all(|service| service["observed_state"] == "running")
        );
    }

    let alpha_worker = stored_alias(prefix.path(), "alpha", "worker");
    let alpha_api = stored_alias(prefix.path(), "alpha", "api");
    let beta_api = stored_alias(prefix.path(), "beta", "api");
    let starts = fake_events(prefix.path());
    assert!(
        event_position(&starts, "start", &alpha_worker)
            < event_position(&starts, "start", &alpha_api),
        "dependency must start before its dependent: {starts:?}"
    );

    let logs = run_with_prefix(prefix.path(), &["logs", "alpha", "api", "--tail", "1"]);
    assert!(logs.status.success(), "{logs:?}");
    let logs = output_json(&logs);
    assert_eq!(logs["stack"], "alpha");
    assert_eq!(logs["service"], "api");
    assert_eq!(logs["tail"], 1);
    assert_eq!(
        logs["stdout"],
        serde_json::json!([format!("fake stdout {alpha_api}")])
    );
    assert_eq!(
        logs["stderr"],
        serde_json::json!([format!("fake stderr {alpha_api}")])
    );

    let starts_before = event_count(&fake_events(prefix.path()), "start", &beta_api);
    let restart = run_with_prefix(prefix.path(), &["restart", "beta", "api"]);
    assert!(restart.status.success(), "{restart:?}");
    assert_eq!(
        output_json(&restart)["services"][0]["observed_state"],
        "running"
    );
    assert_eq!(
        event_count(&fake_events(prefix.path()), "start", &beta_api),
        starts_before + 1,
        "restart must reuse and start the same rootfs alias"
    );

    let down_alpha = run_with_prefix(prefix.path(), &["down", "alpha"]);
    assert!(down_alpha.status.success(), "{down_alpha:?}");
    let down_status = output_json(&down_alpha);
    assert_eq!(down_status["observed_state"], "stopped");
    assert!(
        down_status["services"]
            .as_array()
            .expect("service array")
            .iter()
            .all(|service| service["observed_state"] == "stopped")
    );
    let stopped = fake_events(prefix.path());
    assert!(
        event_position(&stopped, "kill", &alpha_api)
            < event_position(&stopped, "kill", &alpha_worker),
        "dependent must stop before its dependency: {stopped:?}"
    );

    let beta_status = run_with_prefix(prefix.path(), &["status", "beta"]);
    assert!(beta_status.status.success(), "{beta_status:?}");
    assert_eq!(output_json(&beta_status)["observed_state"], "running");

    let down_beta = run_with_prefix(prefix.path(), &["down", "beta"]);
    assert!(down_beta.status.success(), "{down_beta:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn partial_up_cleans_every_started_service_before_failure_is_terminal() {
    let prefix = TestPrefix::new("partial-up-cleanup");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve test port");
    let port = listener.local_addr().expect("reserved address").port();
    let manifest = write_fake_port_blocked_manifest(prefix.path(), port);
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let failed = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    assert!(
        text(&failed.stderr).contains("[port_unavailable]"),
        "{failed:?}"
    );

    let worker = stored_alias(prefix.path(), "partial", "worker");
    let api = stored_alias(prefix.path(), "partial", "api");
    let events = fake_events(prefix.path());
    assert_eq!(event_count(&events, "start", &worker), 1);
    assert_eq!(event_count(&events, "kill", &worker), 1);
    assert_eq!(event_count(&events, "start", &api), 0);
    assert!(active_fake_sessions(prefix.path()).is_empty(), "{events:?}");

    drop(listener);
    let retry = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(retry.status.success(), "{retry:?}");
    let retry_events = fake_events(prefix.path());
    assert_eq!(event_count(&retry_events, "install", &worker), 1);
    assert_eq!(event_count(&retry_events, "install", &api), 1);
    assert_eq!(event_count(&retry_events, "start", &worker), 2);
    assert_eq!(event_count(&retry_events, "start", &api), 1);

    let down = run_with_prefix(prefix.path(), &["down", "partial"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn same_manifest_up_starts_only_the_proven_missing_service() {
    let prefix = TestPrefix::new("mixed-convergence");
    let manifest = write_fake_mixed_manifest(prefix.path());
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let first = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(first.status.success(), "{first:?}");
    let worker = stored_alias(prefix.path(), "mixed", "worker");
    let api = stored_alias(prefix.path(), "mixed", "api");
    wait_for_service_status(prefix.path(), "mixed", "api", |service| {
        service["observed_state"] == "failed"
    });

    let converged = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(converged.status.success(), "{converged:?}");
    let events = fake_events(prefix.path());
    assert_eq!(event_count(&events, "start", &worker), 1);
    assert_eq!(event_count(&events, "start", &api), 2);

    let down = run_with_prefix(prefix.path(), &["down", "mixed"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn automatic_restart_stays_pending_while_a_direct_dependency_is_down() {
    let prefix = TestPrefix::new("blocked-restart");
    let manifest = write_fake_blocked_restart_manifest(prefix.path());
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    let api = stored_alias(prefix.path(), "blocked-restart", "api");
    wait_for_service_status(prefix.path(), "blocked-restart", "api", |service| {
        service["observed_state"] == "restarting"
    });
    thread::sleep(Duration::from_millis(1_200));
    for _ in 0..3 {
        let status = run_with_prefix(prefix.path(), &["status", "blocked-restart"]);
        assert!(status.status.success(), "{status:?}");
        let service = output_json(&status)["services"]
            .as_array()
            .expect("service array")
            .iter()
            .find(|service| service["name"] == "api")
            .expect("api service")
            .clone();
        assert_eq!(service["observed_state"], "restarting");
        assert!(!service["next_restart_at"].is_null());
    }
    assert_eq!(event_count(&fake_events(prefix.path()), "start", &api), 1);

    let down = run_with_prefix(prefix.path(), &["down", "blocked-restart"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn manual_restart_does_not_stop_a_service_with_a_down_dependency() {
    let prefix = TestPrefix::new("blocked-manual-restart");
    let manifest = write_fake_blocked_manual_restart_manifest(prefix.path());
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    let api = stored_alias(prefix.path(), "blocked-manual", "api");
    wait_for_service_status(prefix.path(), "blocked-manual", "worker", |service| {
        service["observed_state"] == "failed"
    });
    let before = fake_events(prefix.path());
    let restart = run_with_prefix(prefix.path(), &["restart", "blocked-manual", "api"]);
    assert_eq!(restart.status.code(), Some(1), "{restart:?}");
    assert!(text(&restart.stderr).contains("[conflict]"), "{restart:?}");
    let after = fake_events(prefix.path());
    assert_eq!(
        event_count(&after, "start", &api),
        event_count(&before, "start", &api)
    );
    assert_eq!(
        event_count(&after, "kill", &api),
        event_count(&before, "kill", &api)
    );

    let down = run_with_prefix(prefix.path(), &["down", "blocked-manual"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn graceful_shutdown_converts_backoff_and_resumes_the_whole_stack() {
    let prefix = TestPrefix::new("shutdown-backoff");
    let manifest = write_fake_shutdown_backoff_manifest(prefix.path());
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "first");
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    wait_for_service_status(prefix.path(), "shutdown-backoff", "api", |service| {
        service["observed_state"] == "restarting"
    });
    assert!(daemon.terminate_and_wait().success());

    let mut restarted = DaemonProcess::spawn(prefix.path(), "restarted");
    wait_until_ready(&mut restarted, &socket);
    let status = run_with_prefix(prefix.path(), &["status", "shutdown-backoff"]);
    assert!(status.status.success(), "{status:?}");
    assert!(
        output_json(&status)["services"]
            .as_array()
            .expect("service array")
            .iter()
            .all(|service| service["observed_state"] == "running")
    );
    let worker = stored_alias(prefix.path(), "shutdown-backoff", "worker");
    let api = stored_alias(prefix.path(), "shutdown-backoff", "api");
    let events = fake_events(prefix.path());
    assert_eq!(event_count(&events, "start", &worker), 2);
    assert_eq!(event_count(&events, "start", &api), 2);

    let down = run_with_prefix(prefix.path(), &["down", "shutdown-backoff"]);
    assert!(down.status.success(), "{down:?}");
    assert!(restarted.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn an_unqualified_spawn_is_quarantined_and_never_restarted() {
    let prefix = TestPrefix::new("unqualified-spawn");
    let manifest = write_fake_hidden_session_manifest(prefix.path());
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert_eq!(up.status.code(), Some(1), "{up:?}");
    assert!(text(&up.stderr).contains("[unknown]"), "{up:?}");
    let alias = stored_alias(prefix.path(), "unqualified", "app");
    thread::sleep(Duration::from_millis(1_200));
    for _ in 0..3 {
        let status = run_with_prefix(prefix.path(), &["status", "unqualified"]);
        assert!(status.status.success(), "{status:?}");
        assert_eq!(
            output_json(&status)["services"][0]["observed_state"],
            "unknown"
        );
    }
    let events = fake_events(prefix.path());
    assert_eq!(event_count(&events, "start", &alias), 1, "{events:?}");
    let pid = events
        .iter()
        .find(|event| event.0 == "start" && event.1 == alias)
        .expect("hidden start event")
        .2
        .parse::<i32>()
        .expect("session PID");
    // SAFETY: the PID comes from the test-owned fake engine session.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    wait_for_no_active_fake_sessions(prefix.path());
    let status = run_with_prefix(prefix.path(), &["status", "unqualified"]);
    assert!(status.status.success(), "{status:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn image_update_rotates_rootfs_generation_and_preserves_named_volume() {
    let prefix = TestPrefix::new("image-update");
    let manifest = write_fake_update_manifest(prefix.path(), "fake:v1");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let first_up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(first_up.status.success(), "{first_up:?}");
    assert_eq!(output_json(&first_up)["revision"], 1);
    let first_alias = stored_alias(prefix.path(), "update", "app");
    let volume = prefix
        .path()
        .join("var/lib/termux-stacks/volumes/update/data");
    let first_metadata = fs::metadata(&volume).expect("inspect first volume generation");
    let marker = volume.join("marker.txt");
    fs::write(&marker, b"persistent data").expect("write named-volume marker");

    write_fake_update_manifest(prefix.path(), "fake:v2");
    let installs_before = fake_events(prefix.path())
        .iter()
        .filter(|event| event.0 == "install")
        .count();
    let running_update = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert_eq!(running_update.status.code(), Some(1), "{running_update:?}");
    assert!(
        text(&running_update.stderr).contains("[conflict]"),
        "{running_update:?}"
    );
    assert_eq!(
        fake_events(prefix.path())
            .iter()
            .filter(|event| event.0 == "install")
            .count(),
        installs_before,
        "a rejected running update must not install a candidate"
    );
    assert_eq!(stored_alias(prefix.path(), "update", "app"), first_alias);

    let stop_before_update = run_with_prefix(prefix.path(), &["down", "update"]);
    assert!(
        stop_before_update.status.success(),
        "{stop_before_update:?}"
    );
    let second_up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(second_up.status.success(), "{second_up:?}");
    assert_eq!(output_json(&second_up)["revision"], 2);
    let second_alias = stored_alias(prefix.path(), "update", "app");
    assert_ne!(second_alias, first_alias, "an update needs a new alias");
    assert_eq!(
        fs::read(&marker).expect("read named-volume marker after update"),
        b"persistent data"
    );
    let second_metadata = fs::metadata(&volume).expect("inspect updated volume generation");
    assert_eq!(
        (second_metadata.dev(), second_metadata.ino()),
        (first_metadata.dev(), first_metadata.ino()),
        "an update must reuse the same named-volume directory"
    );

    assert_eq!(
        stored_generations(prefix.path(), "update", "app"),
        vec![
            StoredGeneration {
                generation: 1,
                alias: first_alias,
                image: "fake:v1".to_owned(),
                state: "installed".to_owned(),
                role: "retired".to_owned(),
            },
            StoredGeneration {
                generation: 2,
                alias: second_alias,
                image: "fake:v2".to_owned(),
                state: "installed".to_owned(),
                role: "current".to_owned(),
            },
        ]
    );

    let down = run_with_prefix(prefix.path(), &["down", "update"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn restart_policies_distinguish_clean_and_failed_exits() {
    let prefix = TestPrefix::new("restart-policies");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let cases = [
        ("policy-no", "no", "exit-nonzero", false, 23),
        ("policy-on-clean", "on-failure", "exit-zero", false, 0),
        ("policy-on-fail", "on-failure", "fail-once", true, 23),
        ("policy-always", "always", "clean-once", true, 0),
    ];

    for (stack, policy, behavior, restarts, first_exit) in cases {
        let manifest = write_fake_policy_manifest(prefix.path(), stack, policy, behavior);
        let up = run_with_prefix(
            prefix.path(),
            &["up", manifest.to_str().expect("UTF-8 path")],
        );
        assert!(up.status.success(), "{up:?}");
        let alias = stored_alias(prefix.path(), stack, "app");

        let service = if restarts {
            wait_for_service_status(prefix.path(), stack, "app", |service| {
                service["observed_state"] == "running" && service["restart_attempts"] == 1
            })
        } else {
            wait_for_service_status(prefix.path(), stack, "app", |service| {
                service["observed_state"] == "failed"
            })
        };
        assert_eq!(service["last_exit_code"], first_exit);
        assert_eq!(service["last_exit_signal"], serde_json::Value::Null);
        assert_eq!(
            event_count(&fake_events(prefix.path()), "start", &alias),
            if restarts { 2 } else { 1 },
            "unexpected start count for restart policy {policy:?}"
        );
        if !restarts {
            assert_eq!(service["restart_attempts"], 0);
            assert_eq!(service["next_restart_at"], serde_json::Value::Null);
        }

        let down = run_with_prefix(prefix.path(), &["down", stack]);
        assert!(down.status.success(), "{down:?}");
    }

    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn crash_loop_stops_after_five_immediate_retries() {
    let prefix = TestPrefix::new("restart-cap");
    let manifest =
        write_fake_policy_manifest(prefix.path(), "restart-cap", "on-failure", "exit-nonzero");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn_with_immediate_restart(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    let alias = stored_alias(prefix.path(), "restart-cap", "app");
    let exhausted = wait_for_service_status(prefix.path(), "restart-cap", "app", |service| {
        service["observed_state"] == "failed"
            && service["restart_attempts"] == 5
            && service["next_restart_at"].is_null()
    });
    assert_eq!(exhausted["last_exit_code"], 23);
    assert_eq!(exhausted["last_exit_signal"], serde_json::Value::Null);
    assert_eq!(
        event_count(&fake_events(prefix.path()), "start", &alias),
        6,
        "the crash loop must contain the initial start and exactly five retries"
    );

    for _ in 0..10 {
        let status = run_with_prefix(prefix.path(), &["status", "restart-cap"]);
        assert!(status.status.success(), "{status:?}");
        let status = output_json(&status);
        let service = &status["services"][0];
        assert_eq!(service["observed_state"], "failed");
        assert_eq!(service["restart_attempts"], 5);
        assert_eq!(service["next_restart_at"], serde_json::Value::Null);
        thread::sleep(POLL_INTERVAL);
    }
    assert_eq!(
        event_count(&fake_events(prefix.path()), "start", &alias),
        6,
        "ticks after cap exhaustion must not start another child"
    );

    let down = run_with_prefix(prefix.path(), &["down", "restart-cap"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn cold_recovery_retries_only_a_proven_pre_engine_intent() {
    let prefix = TestPrefix::new("recover-intent");
    let manifest = write_fake_manifest(prefix.path(), "recover-intent");
    let fault_dir = prepare_fault_dir(prefix.path(), &["before_intent"]);
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn_with_fault(prefix.path(), "faulted", Some(&fault_dir));
    wait_until_ready(&mut daemon, &socket);

    let mut up = spawn_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    wait_for_path(&fault_dir.join("after_intent.reached"));
    daemon.kill_and_wait();
    assert!(!up.wait().expect("wait for interrupted up").success());
    let original_alias = stored_alias(prefix.path(), "recover-intent", "app");

    let mut restarted = DaemonProcess::spawn(prefix.path(), "restarted");
    wait_until_ready(&mut restarted, &socket);
    let recovered = run_with_prefix(prefix.path(), &["status", "recover-intent"]);
    assert!(recovered.status.success(), "{recovered:?}");
    let recovered_status = output_json(&recovered);
    assert_eq!(recovered_status["services"][0]["observed_state"], "failed");
    assert_eq!(recovered_status["services"][0]["rootfs_state"], "absent");

    let retry = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(retry.status.success(), "{retry:?}");
    assert_eq!(
        output_json(&retry)["services"][0]["observed_state"],
        "running"
    );
    assert_ne!(
        stored_alias(prefix.path(), "recover-intent", "app"),
        original_alias
    );

    let down = run_with_prefix(prefix.path(), &["down", "recover-intent"]);
    assert!(down.status.success(), "{down:?}");
    assert!(restarted.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn cold_recovery_reuses_a_proven_installed_rootfs() {
    let prefix = TestPrefix::new("recover-install");
    let manifest = write_fake_manifest(prefix.path(), "recover-install");
    let fault_dir = prepare_fault_dir(prefix.path(), &["before_intent", "after_intent"]);
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn_with_fault(prefix.path(), "faulted", Some(&fault_dir));
    wait_until_ready(&mut daemon, &socket);

    let mut up = spawn_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    wait_for_path(&fault_dir.join("after_install.reached"));
    let installed_alias = stored_alias(prefix.path(), "recover-install", "app");
    daemon.kill_and_wait();
    assert!(!up.wait().expect("wait for interrupted up").success());

    let mut restarted = DaemonProcess::spawn(prefix.path(), "restarted");
    wait_until_ready(&mut restarted, &socket);
    let recovered = run_with_prefix(prefix.path(), &["status", "recover-install"]);
    assert!(recovered.status.success(), "{recovered:?}");
    let recovered_status = output_json(&recovered);
    assert_eq!(recovered_status["services"][0]["observed_state"], "failed");
    assert_eq!(recovered_status["services"][0]["rootfs_state"], "absent");
    let retained = stored_generations(prefix.path(), "recover-install", "app");
    assert_eq!(
        retained.last().expect("retained candidate").state.as_str(),
        "installed"
    );
    assert_eq!(
        retained.last().expect("retained candidate").role.as_str(),
        "candidate"
    );

    let retry = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(retry.status.success(), "{retry:?}");
    assert_eq!(
        stored_alias(prefix.path(), "recover-install", "app"),
        installed_alias
    );

    let down = run_with_prefix(prefix.path(), &["down", "recover-install"]);
    assert!(down.status.success(), "{down:?}");
    assert!(restarted.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn raw_up_replay_uses_the_complete_payload_and_read_requests_cannot_poison_it() {
    let prefix = TestPrefix::new("raw-replay");
    let manifest = write_fake_manifest(prefix.path(), "raw-replay");
    let source = fs::read_to_string(&manifest).expect("read raw replay manifest");
    let base = prefix.path().to_str().expect("UTF-8 test prefix");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let request = serde_json::json!({
        "command": "up",
        "protocol_version": 2,
        "request_id": "raw-replay-id",
        "manifest": source,
        "manifest_base": base,
    });
    let first = raw_protocol_request(prefix.path(), &request);
    assert_eq!(first["ok"], true, "{first}");
    let alias = stored_alias(prefix.path(), "raw-replay", "app");
    assert_eq!(
        event_count(&fake_events(prefix.path()), "install", &alias),
        1
    );
    assert_eq!(event_count(&fake_events(prefix.path()), "start", &alias), 1);

    let status = raw_protocol_request(
        prefix.path(),
        &serde_json::json!({
            "command": "status",
            "protocol_version": 2,
            "request_id": "raw-replay-id",
            "stack": "raw-replay",
        }),
    );
    assert_eq!(status["ok"], true, "{status}");
    let logs = raw_protocol_request(
        prefix.path(),
        &serde_json::json!({
            "command": "logs",
            "protocol_version": 2,
            "request_id": "raw-replay-id",
            "stack": "raw-replay",
            "service": "app",
            "tail": 1,
        }),
    );
    assert_eq!(logs["ok"], true, "{logs}");

    let exact = raw_protocol_request(prefix.path(), &request);
    assert_eq!(exact, first, "exact replay must return the cached response");

    let changed_source = source.replace("fake:latest", "fake:changed");
    let manifest_conflict = raw_protocol_request(
        prefix.path(),
        &serde_json::json!({
            "command": "up",
            "protocol_version": 2,
            "request_id": "raw-replay-id",
            "manifest": changed_source,
            "manifest_base": base,
        }),
    );
    assert_eq!(
        manifest_conflict["error"]["code"], "request_id_conflict",
        "{manifest_conflict}"
    );
    let base_conflict = raw_protocol_request(
        prefix.path(),
        &serde_json::json!({
            "command": "up",
            "protocol_version": 2,
            "request_id": "raw-replay-id",
            "manifest": source,
            "manifest_base": prefix.path().join("different-base"),
        }),
    );
    assert_eq!(
        base_conflict["error"]["code"], "request_id_conflict",
        "{base_conflict}"
    );
    let exact_after_conflicts = raw_protocol_request(prefix.path(), &request);
    assert_eq!(
        exact_after_conflicts, first,
        "conflicting and read-only requests must not overwrite the cached up response"
    );

    let events = fake_events(prefix.path());
    assert_eq!(event_count(&events, "install", &alias), 1, "{events:?}");
    assert_eq!(event_count(&events, "start", &alias), 1, "{events:?}");

    let restart_request = serde_json::json!({
        "command": "restart",
        "protocol_version": 2,
        "request_id": "raw-restart-id",
        "stack": "raw-replay",
        "service": "app",
    });
    let restart = raw_protocol_request(prefix.path(), &restart_request);
    assert_eq!(restart["ok"], true, "{restart}");
    let restart_exact = raw_protocol_request(prefix.path(), &restart_request);
    assert_eq!(restart_exact, restart);
    let target_conflict = raw_protocol_request(
        prefix.path(),
        &serde_json::json!({
            "command": "restart",
            "protocol_version": 2,
            "request_id": "raw-restart-id",
            "stack": "raw-replay",
            "service": "other",
        }),
    );
    assert_eq!(
        target_conflict["error"]["code"], "request_id_conflict",
        "{target_conflict}"
    );
    assert_eq!(
        raw_protocol_request(prefix.path(), &restart_request),
        restart,
        "target mismatch must not overwrite the cached restart response"
    );
    let restart_events = fake_events(prefix.path());
    assert_eq!(event_count(&restart_events, "install", &alias), 1);
    assert_eq!(event_count(&restart_events, "start", &alias), 2);
    assert_eq!(event_count(&restart_events, "kill", &alias), 1);

    let down = run_with_prefix(prefix.path(), &["down", "raw-replay"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn before_commit_sigkill_reconciles_the_parent_without_replaying_any_child_effect() {
    let prefix = TestPrefix::new("before-commit-parent");
    let manifest = write_fake_multi_manifest(prefix.path(), "before-commit-parent");
    let fault_dir = prepare_fault_dir(
        prefix.path(),
        &[
            "before_intent",
            "after_intent",
            "after_install",
            "after_start",
            "between_service_starts",
        ],
    );
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn_with_fault(prefix.path(), "faulted", Some(&fault_dir));
    wait_until_ready(&mut daemon, &socket);

    let mut up = spawn_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    wait_for_path(&fault_dir.join("before_commit.reached"));
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operations WHERE stack_name = 'before-commit-parent' AND outcome IS NULL",
        ),
        1
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operation_services WHERE stack_name = 'before-commit-parent' AND outcome = 'success'",
        ),
        2
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operation_services WHERE stack_name = 'before-commit-parent' AND outcome IS NULL",
        ),
        0,
        "only the parent operation must remain unfinished at before_commit"
    );
    let worker = stored_alias(prefix.path(), "before-commit-parent", "worker");
    let api = stored_alias(prefix.path(), "before-commit-parent", "api");
    let sessions_before = active_fake_session_ids(prefix.path());
    assert_eq!(sessions_before.len(), 2);
    let events_before = fake_events(prefix.path());
    assert_eq!(event_count(&events_before, "start", &worker), 1);
    assert_eq!(event_count(&events_before, "start", &api), 1);

    daemon.kill_and_wait();
    assert!(!up.wait().expect("wait for interrupted up").success());
    let mut restarted = DaemonProcess::spawn(prefix.path(), "restarted");
    wait_until_ready(&mut restarted, &socket);

    let recovered = run_with_prefix(prefix.path(), &["status", "before-commit-parent"]);
    assert!(recovered.status.success(), "{recovered:?}");
    let recovered = output_json(&recovered);
    assert_eq!(recovered["observed_state"], "unknown");
    assert!(
        recovered["services"]
            .as_array()
            .expect("service array")
            .iter()
            .all(|service| service["observed_state"] == "unknown")
    );
    assert_eq!(
        database_text(
            prefix.path(),
            "SELECT phase || ':' || outcome FROM operations WHERE stack_name = 'before-commit-parent' AND operation = 'up' ORDER BY rowid DESC LIMIT 1",
        ),
        "unknown:failure"
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operations WHERE stack_name = 'before-commit-parent'",
        ),
        1
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operation_services WHERE stack_name = 'before-commit-parent' AND phase = 'running' AND outcome = 'success'",
        ),
        2,
        "cold recovery must terminalize only the unfinished parent"
    );
    assert_eq!(active_fake_session_ids(prefix.path()), sessions_before);
    let events_after = fake_events(prefix.path());
    assert_eq!(event_count(&events_after, "install", &worker), 1);
    assert_eq!(event_count(&events_after, "install", &api), 1);
    assert_eq!(event_count(&events_after, "start", &worker), 1);
    assert_eq!(event_count(&events_after, "start", &api), 1);
    assert_eq!(event_count(&events_after, "kill", &worker), 0);
    assert_eq!(event_count(&events_after, "kill", &api), 0);

    assert!(restarted.terminate_and_wait().success());
    signal_fake_sessions(prefix.path(), &sessions_before);
    wait_for_no_active_fake_sessions(prefix.path());
}

#[cfg(target_os = "linux")]
#[test]
fn online_down_failure_terminalizes_the_parent_and_does_not_wedge_the_daemon() {
    let prefix = TestPrefix::new("online-down-failure");
    let manifest = write_fake_multi_manifest(prefix.path(), "down-failure");
    let next_manifest = write_fake_manifest(prefix.path(), "after-down-failure");
    let fault_dir = prepare_fault_dir(
        prefix.path(),
        &[
            "before_intent",
            "after_intent",
            "after_install",
            "after_start",
            "between_service_starts",
            "before_commit",
            "during_backoff",
        ],
    );
    fs::write(fault_dir.join("during_down.fail"), b"").expect("arm down failure checkpoint");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn_with_fault(prefix.path(), "daemon", Some(&fault_dir));
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    let worker = stored_alias(prefix.path(), "down-failure", "worker");
    let api = stored_alias(prefix.path(), "down-failure", "api");
    let failed_sessions = active_fake_session_ids(prefix.path());
    assert_eq!(failed_sessions.len(), 2);

    let down = run_with_prefix(prefix.path(), &["down", "down-failure"]);
    assert_eq!(down.status.code(), Some(1), "{down:?}");
    assert!(text(&down.stderr).contains("[io]"), "{down:?}");
    assert_eq!(
        database_text(
            prefix.path(),
            "SELECT phase || ':' || outcome FROM operations WHERE stack_name = 'down-failure' AND operation = 'down' ORDER BY rowid DESC LIMIT 1",
        ),
        "unknown:failure"
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operations WHERE stack_name = 'down-failure' AND outcome IS NULL",
        ),
        0,
        "the failed down must not leave an unfinished parent"
    );
    let failed_status = run_with_prefix(prefix.path(), &["status", "down-failure"]);
    assert!(failed_status.status.success(), "{failed_status:?}");
    let failed_status = output_json(&failed_status);
    assert_eq!(failed_status["desired_state"], "stopped");
    assert_eq!(failed_status["observed_state"], "unknown");
    assert!(
        failed_status["services"]
            .as_array()
            .expect("service array")
            .iter()
            .all(|service| service["observed_state"] == "unknown")
    );
    assert_eq!(active_fake_session_ids(prefix.path()), failed_sessions);
    let failed_events = fake_events(prefix.path());
    assert_eq!(event_count(&failed_events, "kill", &worker), 0);
    assert_eq!(event_count(&failed_events, "kill", &api), 0);

    let next = run_with_prefix(
        prefix.path(),
        &["up", next_manifest.to_str().expect("UTF-8 path")],
    );
    assert!(next.status.success(), "next mutation was wedged: {next:?}");
    assert_eq!(output_json(&next)["observed_state"], "running");

    signal_fake_sessions(prefix.path(), &failed_sessions);
    wait_for_fake_session_count(prefix.path(), 1);
    let tick = run_with_prefix(prefix.path(), &["status", "after-down-failure"]);
    assert!(tick.status.success(), "{tick:?}");
    fs::remove_file(fault_dir.join("during_down.fail")).expect("disarm down failure checkpoint");
    fs::write(fault_dir.join("during_down.continue"), b"").expect("allow cleanup down checkpoint");
    let next_down = run_with_prefix(prefix.path(), &["down", "after-down-failure"]);
    assert!(next_down.status.success(), "{next_down:?}");
    assert!(daemon.terminate_and_wait().success());
    wait_for_no_active_fake_sessions(prefix.path());
}

#[cfg(target_os = "linux")]
#[test]
fn automatic_restart_resource_failure_restores_backoff_without_an_engine_start() {
    let prefix = TestPrefix::new("restart-resource-backoff");
    let reservation = TcpListener::bind(("127.0.0.1", 0)).expect("reserve restart port");
    let port = reservation
        .local_addr()
        .expect("restart port address")
        .port();
    drop(reservation);
    let manifest = write_fake_restart_port_manifest(prefix.path(), port);
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "daemon");
    wait_until_ready(&mut daemon, &socket);

    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");
    let alias = stored_alias(prefix.path(), "restart-resource", "app");
    let blocker = TcpListener::bind(("127.0.0.1", port)).expect("occupy restart port");
    let scheduled = wait_for_service_status(prefix.path(), "restart-resource", "app", |service| {
        service["observed_state"] == "restarting"
            && service["effect_phase"] == "backoff"
            && service["next_restart_at"].as_i64().is_some()
    });
    let first_deadline = scheduled["next_restart_at"]
        .as_i64()
        .expect("initial restart deadline");
    let restored = wait_for_service_status(prefix.path(), "restart-resource", "app", |service| {
        service["observed_state"] == "restarting"
            && service["effect_phase"] == "backoff"
            && service["next_restart_at"]
                .as_i64()
                .is_some_and(|deadline| deadline > first_deadline)
    });
    assert_eq!(restored["restart_attempts"], 1);
    assert_ne!(restored["observed_state"], "unknown");
    let restored_deadline = restored["next_restart_at"]
        .as_i64()
        .expect("restored restart deadline");
    let messages_before = daemon
        .stderr_text()
        .matches("remains pending after a pre-spawn error")
        .count();
    thread::sleep(Duration::from_millis(200));
    let stable = run_with_prefix(prefix.path(), &["status", "restart-resource"]);
    assert!(stable.status.success(), "{stable:?}");
    let stable = output_json(&stable);
    let service = &stable["services"][0];
    assert_eq!(service["observed_state"], "restarting");
    assert_eq!(service["effect_phase"], "backoff");
    assert!(
        service["next_restart_at"]
            .as_i64()
            .is_some_and(|deadline| deadline >= restored_deadline)
    );
    let messages_after = daemon
        .stderr_text()
        .matches("remains pending after a pre-spawn error")
        .count();
    assert!(
        messages_after.saturating_sub(messages_before) <= 1,
        "restart preflight hot-looped: before={messages_before}, after={messages_after}"
    );
    let blocked_events = fake_events(prefix.path());
    assert_eq!(event_count(&blocked_events, "install", &alias), 1);
    assert_eq!(event_count(&blocked_events, "start", &alias), 1);

    drop(blocker);
    let running = wait_for_service_status(prefix.path(), "restart-resource", "app", |service| {
        service["observed_state"] == "running"
    });
    assert_eq!(running["effect_phase"], "committed");
    assert_eq!(event_count(&fake_events(prefix.path()), "start", &alias), 2);

    let down = run_with_prefix(prefix.path(), &["down", "restart-resource"]);
    assert!(down.status.success(), "{down:?}");
    assert!(daemon.terminate_and_wait().success());
}

#[cfg(target_os = "linux")]
#[test]
fn repeated_graceful_resume_cycles_use_unique_internal_request_ids() {
    let prefix = TestPrefix::new("graceful-resume-ids");
    let manifest = write_fake_manifest(prefix.path(), "graceful-resume-ids");
    let socket = prefix.path().join("var/run/termux-stacks/daemon.sock");
    let mut daemon = DaemonProcess::spawn(prefix.path(), "initial");
    wait_until_ready(&mut daemon, &socket);
    let up = run_with_prefix(
        prefix.path(),
        &["up", manifest.to_str().expect("UTF-8 path")],
    );
    assert!(up.status.success(), "{up:?}");

    for cycle in 0..3 {
        assert!(
            daemon.terminate_and_wait().success(),
            "graceful stop failed at cycle {cycle}: {}",
            daemon.diagnostics()
        );
        wait_for_no_active_fake_sessions(prefix.path());
        drop(daemon);
        daemon = DaemonProcess::spawn(prefix.path(), &format!("resume-{cycle}"));
        wait_until_ready(&mut daemon, &socket);
        let running =
            wait_for_service_status(prefix.path(), "graceful-resume-ids", "app", |service| {
                service["observed_state"] == "running"
            });
        assert_eq!(running["effect_phase"], "committed");
    }

    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operations WHERE stack_name = 'graceful-resume-ids' AND operation = 'resume'",
        ),
        3
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(DISTINCT request_id) FROM operations WHERE stack_name = 'graceful-resume-ids' AND operation = 'resume'",
        ),
        3
    );
    assert_eq!(
        database_i64(
            prefix.path(),
            "SELECT count(*) FROM operations WHERE stack_name = 'graceful-resume-ids' AND operation = 'resume' AND request_id LIKE 'internal-resume-%'",
        ),
        3
    );

    let down = run_with_prefix(prefix.path(), &["down", "graceful-resume-ids"]);
    assert!(down.status.success(), "{down:?}");
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

#[cfg(target_os = "linux")]
fn spawn_with_prefix(prefix: &Path, arguments: &[&str]) -> Child {
    Command::new(binary())
        .args(arguments)
        .env("PREFIX", prefix)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn termux-stacks with PREFIX")
}

#[cfg(target_os = "linux")]
fn raw_protocol_request(prefix: &Path, request: &serde_json::Value) -> serde_json::Value {
    let socket = prefix.join("var/run/termux-stacks/daemon.sock");
    let mut stream = UnixStream::connect(&socket).expect("connect raw protocol client");
    stream
        .set_read_timeout(Some(READY_TIMEOUT))
        .expect("bound raw protocol read");
    stream
        .set_write_timeout(Some(READY_TIMEOUT))
        .expect("bound raw protocol write");
    serde_json::to_writer(&mut stream, request).expect("write raw protocol JSON");
    stream
        .write_all(b"\n")
        .expect("terminate raw protocol frame");
    stream.flush().expect("flush raw protocol frame");

    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read raw protocol response");
    assert!(
        !response.is_empty(),
        "daemon closed raw protocol connection"
    );
    serde_json::from_str(&response).expect("decode raw protocol response")
}

#[cfg(target_os = "linux")]
fn write_fake_manifest(prefix: &Path, stack: &str) -> PathBuf {
    let manifest = prefix.join(format!("{stack}.yaml"));
    fs::write(
        &manifest,
        format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: {stack}\nservices:\n  app:\n    image: fake:latest\n"
        ),
    )
    .expect("write fake manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_multi_manifest(prefix: &Path, stack: &str) -> PathBuf {
    let manifest = prefix.join(format!("{stack}.yaml"));
    fs::write(
        &manifest,
        format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: {stack}\nservices:\n  api:\n    image: fake:latest\n    dependsOn: [worker]\n  worker:\n    image: fake:latest\n"
        ),
    )
    .expect("write multi-service fake manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_port_blocked_manifest(prefix: &Path, port: u16) -> PathBuf {
    let manifest = prefix.join("partial.yaml");
    fs::write(
        &manifest,
        format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: partial\nservices:\n  api:\n    image: fake:latest\n    dependsOn: [worker]\n    ports: [{{address: 127.0.0.1, port: {port}}}]\n  worker:\n    image: fake:latest\n"
        ),
    )
    .expect("write port-blocked manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_mixed_manifest(prefix: &Path) -> PathBuf {
    let manifest = prefix.join("mixed.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: mixed\nservices:\n  api:\n    image: fake:latest\n    dependsOn: [worker]\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: exit-nonzero\n  worker:\n    image: fake:latest\n",
    )
    .expect("write mixed-state manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_blocked_restart_manifest(prefix: &Path) -> PathBuf {
    let manifest = prefix.join("blocked-restart.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: blocked-restart\nservices:\n  api:\n    image: fake:latest\n    dependsOn: [worker]\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: exit-nonzero\n    restart: on-failure\n  worker:\n    image: fake:latest\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: exit-nonzero\n",
    )
    .expect("write dependency-blocked restart manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_blocked_manual_restart_manifest(prefix: &Path) -> PathBuf {
    let manifest = prefix.join("blocked-manual.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: blocked-manual\nservices:\n  api:\n    image: fake:latest\n    dependsOn: [worker]\n  worker:\n    image: fake:latest\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: exit-nonzero\n",
    )
    .expect("write dependency-blocked manual restart manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_shutdown_backoff_manifest(prefix: &Path) -> PathBuf {
    let manifest = prefix.join("shutdown-backoff.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: shutdown-backoff\nservices:\n  api:\n    image: fake:latest\n    dependsOn: [worker]\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: fail-once\n    restart: on-failure\n  worker:\n    image: fake:latest\n",
    )
    .expect("write graceful-shutdown backoff manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_hidden_session_manifest(prefix: &Path) -> PathBuf {
    let manifest = prefix.join("unqualified.yaml");
    fs::write(
        &manifest,
        "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: unqualified\nservices:\n  app:\n    image: fake:latest\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: hidden-session\n    restart: always\n",
    )
    .expect("write hidden-session manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_update_manifest(prefix: &Path, image: &str) -> PathBuf {
    let manifest = prefix.join("update.yaml");
    fs::write(
        &manifest,
        format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: update\nservices:\n  app:\n    image: {image}\n    mounts:\n      - {{type: volume, source: data, target: /data}}\nvolumes:\n  data: {{}}\n"
        ),
    )
    .expect("write update manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_policy_manifest(prefix: &Path, stack: &str, policy: &str, behavior: &str) -> PathBuf {
    let manifest = prefix.join(format!("{stack}.yaml"));
    fs::write(
        &manifest,
        format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: {stack}\nservices:\n  app:\n    image: fake:latest\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: {behavior}\n    restart: {policy}\n"
        ),
    )
    .expect("write restart-policy manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn write_fake_restart_port_manifest(prefix: &Path, port: u16) -> PathBuf {
    let manifest = prefix.join("restart-resource.yaml");
    fs::write(
        &manifest,
        format!(
            "apiVersion: termux-stacks/v1alpha1\nkind: Stack\nmetadata:\n  name: restart-resource\nservices:\n  app:\n    image: fake:latest\n    environment:\n      TERMUX_STACKS_FAKE_BEHAVIOR: fail-once\n    ports: [{{address: 127.0.0.1, port: {port}}}]\n    restart: on-failure\n"
        ),
    )
    .expect("write restart resource manifest");
    manifest
}

#[cfg(target_os = "linux")]
fn prepare_fault_dir(prefix: &Path, continued: &[&str]) -> PathBuf {
    let fault_dir = prefix.join("fault");
    fs::create_dir(&fault_dir).expect("create fault directory");
    fs::set_permissions(&fault_dir, fs::Permissions::from_mode(0o700))
        .expect("make fault directory private");
    for checkpoint in continued {
        fs::write(fault_dir.join(format!("{checkpoint}.continue")), b"")
            .expect("write checkpoint continuation");
    }
    fault_dir
}

#[cfg(target_os = "linux")]
fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "fault checkpoint did not create {}",
            path.display()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(target_os = "linux")]
fn database_i64(prefix: &Path, query: &str) -> i64 {
    let connection = rusqlite::Connection::open(prefix.join("var/lib/termux-stacks/state.db"))
        .expect("open state database");
    connection
        .query_row(query, [], |row| row.get(0))
        .expect("read integer from state database")
}

#[cfg(target_os = "linux")]
fn database_text(prefix: &Path, query: &str) -> String {
    let connection = rusqlite::Connection::open(prefix.join("var/lib/termux-stacks/state.db"))
        .expect("open state database");
    connection
        .query_row(query, [], |row| row.get(0))
        .expect("read text from state database")
}

#[cfg(target_os = "linux")]
fn stored_alias(prefix: &Path, stack: &str, service: &str) -> String {
    let connection = rusqlite::Connection::open(prefix.join("var/lib/termux-stacks/state.db"))
        .expect("open state database");
    connection
        .query_row(
            "SELECT alias FROM rootfs_generations
              WHERE stack_name = ?1 AND service_name = ?2
              ORDER BY generation DESC LIMIT 1",
            [stack, service],
            |row| row.get(0),
        )
        .expect("read stored alias")
}

#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
struct StoredGeneration {
    generation: i64,
    alias: String,
    image: String,
    state: String,
    role: String,
}

#[cfg(target_os = "linux")]
fn stored_generations(prefix: &Path, stack: &str, service: &str) -> Vec<StoredGeneration> {
    let connection = rusqlite::Connection::open(prefix.join("var/lib/termux-stacks/state.db"))
        .expect("open state database");
    let mut statement = connection
        .prepare(
            "SELECT generation, alias, image, state, role
               FROM rootfs_generations
              WHERE stack_name = ?1 AND service_name = ?2
              ORDER BY generation",
        )
        .expect("prepare rootfs generation query");
    statement
        .query_map([stack, service], |row| {
            Ok(StoredGeneration {
                generation: row.get(0)?,
                alias: row.get(1)?,
                image: row.get(2)?,
                state: row.get(3)?,
                role: row.get(4)?,
            })
        })
        .expect("query rootfs generations")
        .collect::<Result<Vec<_>, _>>()
        .expect("read rootfs generations")
}

#[cfg(target_os = "linux")]
fn wait_for_service_status(
    prefix: &Path,
    stack: &str,
    service: &str,
    condition: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_status = serde_json::Value::Null;
    loop {
        let output = run_with_prefix(prefix, &["status", stack]);
        if output.status.success() {
            let status = output_json(&output);
            if let Some(candidate) = status["services"]
                .as_array()
                .and_then(|services| services.iter().find(|entry| entry["name"] == service))
                && condition(candidate)
            {
                return candidate.clone();
            }
            last_status = status;
        }
        assert!(
            Instant::now() < deadline,
            "service {stack:?}/{service:?} did not reach the expected state; last status: {last_status}"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn output_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "command did not return JSON: {error}; stdout={:?}; stderr={:?}",
            text(&output.stdout),
            text(&output.stderr)
        )
    })
}

#[cfg(target_os = "linux")]
fn fake_events(prefix: &Path) -> Vec<(String, String, String)> {
    fs::read_to_string(prefix.join("fake-engine/events"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let action = fields.next().expect("event action");
            let alias = fields.next().expect("event alias");
            let detail = fields.next().expect("event detail");
            assert!(fields.next().is_none(), "unexpected fake event: {line:?}");
            (action.to_owned(), alias.to_owned(), detail.to_owned())
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn event_position(events: &[(String, String, String)], action: &str, alias: &str) -> usize {
    events
        .iter()
        .position(|event| event.0 == action && event.1 == alias)
        .unwrap_or_else(|| panic!("missing fake event {action:?} for {alias:?}: {events:?}"))
}

#[cfg(target_os = "linux")]
fn event_count(events: &[(String, String, String)], action: &str, alias: &str) -> usize {
    events
        .iter()
        .filter(|event| event.0 == action && event.1 == alias)
        .count()
}

#[cfg(target_os = "linux")]
fn active_fake_sessions(prefix: &Path) -> Vec<PathBuf> {
    fs::read_dir(prefix.join("fake-engine/sessions"))
        .map(|entries| {
            entries
                .map(|entry| entry.expect("session entry").path())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn active_fake_session_ids(prefix: &Path) -> Vec<u32> {
    let mut sessions = active_fake_sessions(prefix)
        .into_iter()
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("UTF-8 fake session name")
                .parse::<u32>()
                .expect("numeric fake session name")
        })
        .collect::<Vec<_>>();
    sessions.sort_unstable();
    sessions
}

#[cfg(target_os = "linux")]
fn signal_fake_sessions(prefix: &Path, sessions: &[u32]) {
    let active = active_fake_session_ids(prefix);
    for session in sessions {
        assert!(
            active.contains(session),
            "fake session {session} is not active"
        );
        // SAFETY: every PID comes from a session record created below this
        // test-owned PREFIX by the fake engine process.
        assert_eq!(
            unsafe { libc::kill(*session as i32, libc::SIGTERM) },
            0,
            "terminate test-owned fake session {session}"
        );
    }
}

#[cfg(target_os = "linux")]
fn wait_for_fake_session_count(prefix: &Path, expected: usize) {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let sessions = active_fake_session_ids(prefix);
        if sessions.len() == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fake session count did not become {expected}: {sessions:?}"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(target_os = "linux")]
fn wait_for_no_active_fake_sessions(prefix: &Path) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while !active_fake_sessions(prefix).is_empty() {
        assert!(
            Instant::now() < deadline,
            "fake sessions did not terminate: {:?}",
            active_fake_sessions(prefix)
        );
        thread::sleep(POLL_INTERVAL);
    }
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
        Self::spawn_configured(prefix, label, None, false)
    }

    #[cfg(target_os = "linux")]
    fn spawn_with_fault(prefix: &Path, label: &str, fault_dir: Option<&Path>) -> Self {
        Self::spawn_configured(prefix, label, fault_dir, false)
    }

    #[cfg(target_os = "linux")]
    fn spawn_with_immediate_restart(prefix: &Path, label: &str) -> Self {
        Self::spawn_configured(prefix, label, None, true)
    }

    fn spawn_configured(
        prefix: &Path,
        label: &str,
        fault_dir: Option<&Path>,
        immediate_restart: bool,
    ) -> Self {
        let fake_bin = prefix.join("bin");
        fs::create_dir_all(&fake_bin).expect("create fake engine bin directory");
        let fake_engine = fake_bin.join("proot-distro");
        let fake_state = prefix.join("fake-engine");
        fs::create_dir_all(&fake_state).expect("create fake engine state");
        if !fake_engine.exists() {
            let shell = std::env::var_os("PREFIX")
                .map(|prefix| PathBuf::from(prefix).join("bin/sh"))
                .unwrap_or_else(|| "/bin/sh".into());
            let mut script = format!("#!{}\n", shell.display());
            script.push_str(
                r#"set -eu
state=${TERMUX_STACKS_FAKE_STATE:?}
sessions="$state/sessions"
events="$state/events"
images="$state/images"
attempts="$state/attempts"
hidden="$state/hidden"
mkdir -p "$sessions" "$images" "$attempts" "$hidden"

event() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$events"
}

case ${1:-} in
  help)
    printf '%s\n' 'proot-distro 5.6.0 install run ps kill remove'
    ;;
  install)
    shift
    alias=
    image=
    while [ "$#" -gt 0 ]; do
      case $1 in
        --architecture) shift 2 ;;
        --name) alias=$2; shift 2 ;;
        *) image=$1; shift ;;
      esac
    done
    [ -n "$alias" ] && [ -n "$image" ] || exit 2
    printf '%s\n' "$image" >"$images/$alias"
    event install "$alias" "$image"
    ;;
  run)
    shift
    [ "${1:-}" = --isolated ] || exit 2
    shift
    alias=
    behavior=
    while [ "$#" -gt 0 ]; do
      case $1 in
        --env)
          case ${2:-} in
            TERMUX_STACKS_FAKE_BEHAVIOR=*) behavior=${2#*=} ;;
          esac
          shift 2
          ;;
        --bind) shift 2 ;;
        --*) exit 2 ;;
        *) alias=$1; shift; break ;;
      esac
    done
    [ -n "$alias" ] || exit 2
    if [ -z "$behavior" ] && [ "${1:-}" = -- ]; then
      shift
      case ${1:-} in
        fake-*) behavior=${1#fake-} ;;
      esac
    fi
    if [ -z "$behavior" ] && [ -f "$images/$alias" ]; then
      image=
      IFS= read -r image <"$images/$alias" || :
      case $image in
        fake:exit-zero|fake:exit-nonzero|fake:fail-once|fake:clean-once)
          behavior=${image#fake:}
          ;;
      esac
    fi
    behavior=${behavior:-loop}
    record="$sessions/$$"
    umask 077
    printf '%s\n' "$alias" >"$record"
    if [ "$behavior" = hidden-session ]; then
      : >"$hidden/$$"
    fi
    event start "$alias" "$$"
    printf 'fake stdout %s\n' "$alias"
    printf 'fake stderr %s\n' "$alias" >&2
    cleanup() { rm -f "$record" "$hidden/$$"; }
    trap 'cleanup; exit 0' TERM INT
    trap cleanup EXIT
    case $behavior in
      exit-zero)
        sleep 0.5
        exit 0
        ;;
      exit-nonzero)
        sleep 0.5
        exit 23
        ;;
      fail-once|clean-once)
        attempt=0
        attempt_file="$attempts/$alias"
        if [ -f "$attempt_file" ]; then
          IFS= read -r attempt <"$attempt_file" || attempt=0
        fi
        attempt=$((attempt + 1))
        printf '%s\n' "$attempt" >"$attempt_file"
        if [ "$attempt" -eq 1 ]; then
          sleep 0.5
          if [ "$behavior" = clean-once ]; then
            exit 0
          fi
          exit 23
        fi
        ;;
      loop|hidden-session) ;;
      *) exit 2 ;;
    esac
    while :; do sleep 1; done
    ;;
  ps)
    for record in "$sessions"/*; do
      [ -f "$record" ] || continue
      session=${record##*/}
      [ ! -f "$hidden/$session" ] || continue
      if kill -0 "$session" 2>/dev/null; then
        printf '%s\n' "$session"
      else
        rm -f "$record"
      fi
    done
    ;;
  kill)
    session=${2:?}
    record="$sessions/$session"
    [ -f "$record" ] || exit 1
    alias=unknown
    IFS= read -r alias <"$record" || :
    event kill "$alias" "$session"
    kill -TERM "$session"
    ;;
  remove)
    event remove "${2:-unknown}" 0
    ;;
  *) exit 2 ;;
esac
"#,
            );
            fs::write(&fake_engine, script).expect("write fake engine");
            fs::set_permissions(&fake_engine, fs::Permissions::from_mode(0o700))
                .expect("make fake engine executable");
        }
        let stdout = prefix.join(format!("{label}.stdout"));
        let stderr = prefix.join(format!("{label}.stderr"));
        let stdout_file = fs::File::create(&stdout).expect("create daemon stdout log");
        let stderr_file = fs::File::create(&stderr).expect("create daemon stderr log");
        let mut command = Command::new(binary());
        command
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
            .stderr(Stdio::from(stderr_file));
        if let Some(fault_dir) = fault_dir {
            command.env("TERMUX_STACKS_FAULT_DIR", fault_dir);
        }
        if immediate_restart {
            command.env("TERMUX_STACKS_TEST_IMMEDIATE_RESTART", "1");
        }
        let child = command.spawn().expect("spawn daemon");

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
