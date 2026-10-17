//! The emulators the benchmarks run in Docker, how each one is started, and
//! how the benchmarks decide it is ready.
//!
//! `startup_bench`, `memory_profiler` and `size_comparison` all read this
//! table, so a target's startup time, idle memory and image size always come
//! from the same image and the same `docker run`.

use aws_sdk_dynamodb::config::retry::RetryConfig;
use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use std::fmt;
use std::io::Read;
use std::process::Command;
use std::time::{Duration, Instant};

/// How often readiness is polled. Floci answers its first ListTables in well
/// under 200ms, so a coarser interval would be a visible share of what is
/// measured for it.
pub const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long a container is left alone before its state is first read while
/// waiting for it. A target that answers within this is never touched by the
/// check; one that takes longer pays a `docker inspect` on a separate thread
/// every [`EXIT_CHECK_INTERVAL`] until it does.
pub const EXIT_CHECK_GRACE: Duration = Duration::from_secs(3);

/// How often the container's state is read once the grace period has passed.
pub const EXIT_CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// How many of a failed container's last log lines are printed.
const LOG_TAIL_LINES: &str = "20";

/// One emulator image, and how its project says to run it.
#[derive(Debug)]
pub struct ContainerTarget {
    /// Key used in the result files and README markers, e.g. `floci`.
    pub key: &'static str,
    /// Name for progress output, e.g. `Floci`.
    pub label: &'static str,
    pub image: &'static str,
    /// Suffix for container names, e.g. `dynoxide-bench-` + `ls`.
    pub short_name: &'static str,
    /// Each target gets its own host port, so a container left behind by one
    /// target can never answer another target's readiness check.
    pub host_port: u16,
    pub container_port: u16,
    /// Extra `docker run` options, placed before the image name.
    pub run_options: &'static [&'static str],
    /// Arguments after the image name.
    pub command: &'static [&'static str],
    /// Host environment variables passed into the container when set.
    pub passthrough_env: &'static [&'static str],
}

pub const DYNAMODB_LOCAL: ContainerTarget = ContainerTarget {
    key: "dynamodb_local",
    label: "DynamoDB Local",
    image: "amazon/dynamodb-local:latest",
    short_name: "ddb",
    host_port: 8002,
    container_port: 8000,
    run_options: &[],
    command: &["-jar", "DynamoDBLocal.jar", "-inMemory", "-sharedDb"],
    passthrough_env: &[],
};

/// Runs every service, as it does by default. The latest image needs an auth
/// token to start, which CI supplies.
pub const LOCALSTACK: ContainerTarget = ContainerTarget {
    key: "localstack",
    label: "LocalStack",
    image: "localstack/localstack:latest",
    short_name: "ls",
    host_port: 4566,
    container_port: 4566,
    run_options: &[],
    command: &[],
    passthrough_env: &["LOCALSTACK_AUTH_TOKEN"],
};

/// Floci's docs run it with the Docker socket mounted, and its CLI mounts it
/// by default. Without the socket it retries the connection with backoff
/// before serving anything, and the first ListTables lands about 15.7s after
/// start rather than about 0.2s, which would measure the missing socket
/// rather than Floci. Its entrypoint picks up the socket's group itself, so
/// no `--user` is needed.
pub const FLOCI: ContainerTarget = ContainerTarget {
    key: "floci",
    label: "Floci",
    image: "floci/floci:latest",
    short_name: "floci",
    host_port: 4567,
    container_port: 4566,
    run_options: &["-v", "/var/run/docker.sock:/var/run/docker.sock"],
    command: &[],
    passthrough_env: &[],
};

/// MiniStack's quick start runs the image with no options. Its docs add the
/// Docker socket only for services backed by real containers (RDS, ECS,
/// Lambda), and with or without it DynamoDB answers at the same point.
pub const MINISTACK: ContainerTarget = ContainerTarget {
    key: "ministack",
    label: "MiniStack",
    image: "ministackorg/ministack:latest",
    short_name: "ministack",
    host_port: 4568,
    container_port: 4566,
    run_options: &[],
    command: &[],
    passthrough_env: &[],
};

/// Multi-service emulators, measured for startup, idle memory and image size
/// only, and all in the same way.
pub const MULTI_SERVICE_EMULATORS: [&ContainerTarget; 3] = [&LOCALSTACK, &FLOCI, &MINISTACK];

/// Every container target, in the order the results list them.
pub const ALL: [&ContainerTarget; 4] = [&DYNAMODB_LOCAL, &LOCALSTACK, &FLOCI, &MINISTACK];

impl ContainerTarget {
    pub fn container_name(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.short_name)
    }

    pub fn endpoint(&self) -> String {
        format!("http://localhost:{}", self.host_port)
    }

    /// The arguments for `docker run`. `env` looks up host environment
    /// variables; it is a parameter so the forwarding can be tested.
    ///
    /// There is no `--rm`. With it, Docker deletes a container the moment it
    /// exits, so a target that crashes on start leaves no logs or exit code
    /// to explain the failure. The benchmarks remove every container they
    /// start with [`remove_container`] instead.
    pub fn docker_run_args(
        &self,
        container_name: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "-p".into(),
            format!("{}:{}", self.host_port, self.container_port),
            "--name".into(),
            container_name.into(),
        ];
        args.extend(self.run_options.iter().map(|s| s.to_string()));
        for var in self.passthrough_env {
            if let Some(value) = env(var) {
                args.push("-e".into());
                args.push(format!("{var}={value}"));
            }
        }
        args.push(self.image.into());
        args.extend(self.command.iter().map(|s| s.to_string()));
        args
    }

    /// Start the container in the background. Does not wait for it.
    ///
    /// If `docker run` fails, any container it created (one whose start
    /// failed on a taken port, say) is removed before returning.
    pub fn run(&self, container_name: &str) -> Result<(), String> {
        let args = self.docker_run_args(container_name, |var| std::env::var(var).ok());
        let output = Command::new("docker")
            .args(&args)
            .output()
            .map_err(|e| format!("could not run docker: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            remove_container(container_name);
            Err(format!(
                "docker run failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

/// What Docker says a container is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    /// Stopped, with the exit code the container gave.
    Exited(i64),
    /// Any other status Docker reports, such as `created` or `restarting`.
    Other(String),
    /// No container by that name, or Docker could not say.
    Gone,
}

impl fmt::Display for ContainerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContainerState::Running => f.write_str("running"),
            ContainerState::Exited(code) => write!(f, "exited with code {code}"),
            ContainerState::Other(status) => f.write_str(status),
            ContainerState::Gone => f.write_str("gone"),
        }
    }
}

impl ContainerState {
    /// Parse `docker inspect --format '{{.State.Status}} {{.State.ExitCode}}'`.
    fn parse(text: &str) -> Self {
        let mut fields = text.split_whitespace();
        match (fields.next(), fields.next()) {
            (Some("running"), _) => ContainerState::Running,
            (Some("exited"), Some(code)) => match code.parse() {
                Ok(code) => ContainerState::Exited(code),
                Err(_) => ContainerState::Other(format!("exited ({code})")),
            },
            (Some(status), _) => ContainerState::Other(status.to_string()),
            (None, _) => ContainerState::Gone,
        }
    }
}

pub fn container_state(container_name: &str) -> ContainerState {
    let output = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.State.Status}} {{.State.ExitCode}}",
            container_name,
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            ContainerState::parse(&String::from_utf8_lossy(&output.stdout))
        }
        _ => ContainerState::Gone,
    }
}

/// Poll `endpoint` with ListTables until a call succeeds, giving up once
/// `timeout` has passed. Returns whether a call succeeded.
///
/// The SDK's own retries are off. With them on, a refused connection is
/// retried inside the one call after a backoff of up to a few seconds, so a
/// target that came up during the backoff is only seen when it ends: Floci
/// measured anywhere from 0.2s to 1.0s that way. Every call is also bounded
/// by what is left of `timeout`, so a server that accepts the connection and
/// never answers cannot hold the benchmark past it.
pub async fn wait_for_list_tables(endpoint: &str, timeout: Duration) -> bool {
    let client = probe_client(endpoint);
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, client.list_tables().send()).await {
            Ok(Ok(_)) => return true,
            Ok(Err(_)) => tokio::time::sleep(POLL_INTERVAL).await,
            Err(_) => return false,
        }
    }
}

/// [`wait_for_list_tables`] for a container, which also gives up as soon as the
/// container is seen to have exited, rather than polling a port nothing will
/// ever answer on until `timeout` runs out. LocalStack without its auth token
/// exits with code 55 within seconds, and the 300s wait made every run pay five
/// minutes to say so.
///
/// The container's state is first read after [`EXIT_CHECK_GRACE`] and then
/// every [`EXIT_CHECK_INTERVAL`], on a blocking thread, alongside the polling
/// rather than between its calls, so a probe that is stuck waiting on a dead
/// container's port does not hold the check up. Returns whether a call
/// succeeded; call [`report_not_ready`] for the reason when it did not.
pub async fn wait_for_container_list_tables(
    endpoint: &str,
    container_name: &str,
    timeout: Duration,
) -> bool {
    let name = container_name.to_string();
    wait_for_list_tables_unless_exited(
        endpoint,
        timeout,
        EXIT_CHECK_GRACE,
        EXIT_CHECK_INTERVAL,
        move || container_state(&name),
    )
    .await
}

async fn wait_for_list_tables_unless_exited<F>(
    endpoint: &str,
    timeout: Duration,
    grace: Duration,
    interval: Duration,
    state: F,
) -> bool
where
    F: Fn() -> ContainerState + Clone + Send + 'static,
{
    tokio::select! {
        ready = wait_for_list_tables(endpoint, timeout) => ready,
        () = watch_for_exit(grace, interval, state) => false,
    }
}

/// Resolves once `state` reports the container has exited. Reads it first
/// after `grace`, then every `interval`.
async fn watch_for_exit<F>(grace: Duration, interval: Duration, state: F)
where
    F: Fn() -> ContainerState + Clone + Send + 'static,
{
    tokio::time::sleep(grace).await;
    loop {
        if let Ok(ContainerState::Exited(_)) = tokio::task::spawn_blocking(state.clone()).await {
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

fn probe_client(endpoint: &str) -> aws_sdk_dynamodb::Client {
    let config = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(endpoint)
        .credentials_provider(Credentials::new("fake", "fake", None, None, "bench"))
        .retry_config(RetryConfig::disabled())
        .build();
    aws_sdk_dynamodb::Client::from_conf(config)
}

pub fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether the image is already pulled. The benchmarks skip an image that is
/// not, rather than let `docker run` pull it inside the timed window.
pub fn image_available(image: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn remove_container(container_name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", container_name])
        .output();
}

/// Explain why `target` never answered: print the container's last log lines
/// to stderr and return the reason for the failure summary. Call it before
/// removing the container, or there is nothing left to read.
pub fn report_not_ready(
    target: &ContainerTarget,
    container_name: &str,
    timeout: Duration,
) -> String {
    let state = container_state(container_name);
    print_log_tail(container_name);
    not_ready_reason(target, &state, timeout)
}

/// Explain why a target stopped after it had answered: print the container's
/// last log lines and return the reason. Call it before removing the
/// container.
pub fn report_stopped(
    target: &ContainerTarget,
    container_name: &str,
    state: &ContainerState,
) -> String {
    print_log_tail(container_name);
    format!(
        "{} stopped while it was being measured (container {state})",
        target.label
    )
}

/// Why a target produced no measurement, for the failure summary. An exit
/// code says the target crashed; a container still running says it was too
/// slow or stuck.
fn not_ready_reason(target: &ContainerTarget, state: &ContainerState, timeout: Duration) -> String {
    if let ContainerState::Exited(code) = state {
        return format!(
            "{} exited with code {code} without answering ListTables at {}",
            target.label,
            target.endpoint()
        );
    }
    format!(
        "{} did not answer ListTables at {} within {}s (container {state})",
        target.label,
        target.endpoint(),
        timeout.as_secs()
    )
}

/// Print the container's last log lines to stderr, to explain a failure.
pub fn print_log_tail(container_name: &str) {
    let mut command = Command::new("docker");
    command.args(["logs", "--tail", LOG_TAIL_LINES, container_name]);
    match merged_output(command) {
        Ok(text) if text.trim().is_empty() => {
            eprintln!("  {container_name} wrote nothing to its log");
        }
        Ok(text) => {
            eprintln!("  last log lines from {container_name}:");
            for line in text.trim_end().lines() {
                eprintln!("    {line}");
            }
        }
        Err(e) => eprintln!("  could not read the log of {container_name}: {e}"),
    }
}

/// Run `command` with its stdout and stderr on one pipe, so the output reads
/// in the order it was written. `docker logs` replays the container's stdout
/// and stderr on its own, and reading them separately would print every
/// error after every ordinary line.
///
/// Returns the output, or, if the command fails, what it printed.
fn merged_output(mut command: Command) -> Result<String, String> {
    let (mut reader, writer) = std::io::pipe().map_err(|e| format!("no pipe: {e}"))?;
    let writer_for_stderr = writer.try_clone().map_err(|e| format!("no pipe: {e}"))?;
    let mut child = command
        .stdout(writer)
        .stderr(writer_for_stderr)
        .spawn()
        .map_err(|e| format!("could not run {:?}: {e}", command.get_program()))?;
    // `command` holds the write ends until it is dropped, and the read below
    // only ends once every write end has closed.
    drop(command);
    let mut bytes = Vec::new();
    let read = reader.read_to_end(&mut bytes);
    let status = child.wait().map_err(|e| e.to_string())?;
    read.map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn floci_mounts_the_docker_socket_before_the_image() {
        let args = FLOCI.docker_run_args("bench-floci", no_env);
        let image = args.iter().position(|a| a == FLOCI.image).unwrap();
        let mount = args
            .iter()
            .position(|a| a == "/var/run/docker.sock:/var/run/docker.sock")
            .unwrap();
        assert_eq!(args[mount - 1], "-v");
        assert!(
            mount < image,
            "options after the image would be its command"
        );
    }

    #[test]
    fn ministack_runs_with_no_extra_options() {
        let args = MINISTACK.docker_run_args("bench-ministack", no_env);
        assert_eq!(
            args,
            [
                "run",
                "-d",
                "-p",
                "4568:4566",
                "--name",
                "bench-ministack",
                "ministackorg/ministack:latest",
            ]
        );
    }

    #[test]
    fn localstack_forwards_its_token_only_when_set() {
        let without = LOCALSTACK.docker_run_args("bench-ls", no_env);
        assert!(
            !without
                .iter()
                .any(|a| a.starts_with("LOCALSTACK_AUTH_TOKEN"))
        );

        let with = LOCALSTACK.docker_run_args("bench-ls", |var| {
            (var == "LOCALSTACK_AUTH_TOKEN").then(|| "secret".to_string())
        });
        let token = with
            .iter()
            .position(|a| a == "LOCALSTACK_AUTH_TOKEN=secret")
            .unwrap();
        assert_eq!(with[token - 1], "-e");
        assert_eq!(with.last().unwrap(), LOCALSTACK.image);
    }

    #[test]
    fn dynamodb_local_command_follows_the_image() {
        let args = DYNAMODB_LOCAL.docker_run_args("bench-ddb", no_env);
        let image = args.iter().position(|a| a == DYNAMODB_LOCAL.image).unwrap();
        assert_eq!(
            &args[image + 1..],
            ["-jar", "DynamoDBLocal.jar", "-inMemory", "-sharedDb"]
        );
    }

    #[test]
    fn no_target_is_removed_by_docker_when_it_exits() {
        // A crashed container must still be there to read its logs from.
        for target in ALL {
            let args = target.docker_run_args("bench", no_env);
            assert!(!args.iter().any(|a| a == "--rm"), "{}", target.label);
        }
    }

    #[test]
    fn container_state_parses_docker_inspect() {
        assert_eq!(
            ContainerState::parse("running 0\n"),
            ContainerState::Running
        );
        assert_eq!(
            ContainerState::parse("exited 137\n"),
            ContainerState::Exited(137)
        );
        assert_eq!(
            ContainerState::parse("created 0\n"),
            ContainerState::Other("created".into())
        );
        assert_eq!(ContainerState::parse(""), ContainerState::Gone);
    }

    #[test]
    fn a_crash_is_reported_with_its_exit_code() {
        let reason = not_ready_reason(&FLOCI, &ContainerState::Exited(1), Duration::from_secs(300));
        assert_eq!(
            reason,
            "Floci exited with code 1 without answering ListTables at http://localhost:4567"
        );
    }

    #[test]
    fn a_timeout_says_what_the_container_was_doing() {
        let timeout = Duration::from_secs(300);
        assert_eq!(
            not_ready_reason(&MINISTACK, &ContainerState::Running, timeout),
            "MiniStack did not answer ListTables at http://localhost:4568 within 300s \
             (container running)"
        );
        assert!(
            not_ready_reason(&MINISTACK, &ContainerState::Gone, timeout)
                .ends_with("(container gone)")
        );
    }

    #[test]
    fn merged_output_keeps_stdout_and_stderr_in_the_order_written() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo one; echo two >&2; echo three"]);
        assert_eq!(merged_output(command).unwrap(), "one\ntwo\nthree\n");
    }

    #[test]
    fn merged_output_returns_what_a_failing_command_printed() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo 'No such container: x' >&2; exit 1"]);
        assert_eq!(merged_output(command).unwrap_err(), "No such container: x");
    }

    #[test]
    fn targets_never_share_a_host_port() {
        let mut ports: Vec<u16> = ALL.iter().map(|t| t.host_port).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), ALL.len());
    }

    #[tokio::test]
    async fn readiness_fails_when_nothing_answers() {
        // Bind a port, then drop the listener so connections are refused.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let started = Instant::now();
        let ready = wait_for_list_tables(
            &format!("http://127.0.0.1:{port}"),
            Duration::from_millis(300),
        )
        .await;
        assert!(!ready);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn readiness_gives_up_on_a_server_that_never_answers() {
        // Accepts the connection, then says nothing.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let started = Instant::now();
        let ready =
            wait_for_list_tables(&format!("http://{addr}"), Duration::from_millis(300)).await;
        assert!(!ready);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A port nothing is listening on.
    fn refused_endpoint() -> String {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn a_container_that_has_exited_ends_the_wait_early() {
        let started = Instant::now();
        let ready = wait_for_list_tables_unless_exited(
            &refused_endpoint(),
            Duration::from_secs(300),
            Duration::ZERO,
            Duration::from_millis(10),
            || ContainerState::Exited(3),
        )
        .await;
        assert!(!ready);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "gave up after {:?}, nowhere near the 300s timeout",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn an_exit_ends_the_wait_even_while_a_probe_is_stuck() {
        // A proxy that still accepts connections on a dead container's port:
        // the probe is waiting on an answer that never comes.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let started = Instant::now();
        let ready = wait_for_list_tables_unless_exited(
            &format!("http://{addr}"),
            Duration::from_secs(300),
            Duration::ZERO,
            Duration::from_millis(10),
            || ContainerState::Exited(55),
        )
        .await;
        assert!(!ready);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "gave up after {:?}, nowhere near the 300s timeout",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_container_still_running_is_waited_for_until_the_timeout() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        let started = Instant::now();
        let ready = wait_for_list_tables_unless_exited(
            &refused_endpoint(),
            Duration::from_millis(400),
            Duration::ZERO,
            Duration::from_millis(10),
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                ContainerState::Running
            },
        )
        .await;
        assert!(!ready);
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "a running container ends the wait at the timeout, not before"
        );
        assert!(
            reads.load(Ordering::SeqCst) > 0,
            "the state was read while waiting"
        );
    }

    #[tokio::test]
    async fn the_state_is_not_read_inside_the_grace_period() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        let ready = wait_for_list_tables_unless_exited(
            &refused_endpoint(),
            Duration::from_millis(300),
            Duration::from_secs(60),
            Duration::from_millis(10),
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                ContainerState::Exited(1)
            },
        )
        .await;
        assert!(!ready);
        assert_eq!(reads.load(Ordering::SeqCst), 0, "no read inside the grace");
    }

    #[tokio::test]
    async fn readiness_succeeds_against_a_dynamodb_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let db = dynoxide::Database::memory().unwrap();
        tokio::spawn(async move { dynoxide::server::serve_on(listener, db).await });
        assert!(wait_for_list_tables(&format!("http://{addr}"), Duration::from_secs(10)).await);
    }
}
