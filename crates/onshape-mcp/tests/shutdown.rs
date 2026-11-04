//! SIGTERM/SIGINT shutdown of `onshape-mcp http` (external-token and OAuth mode).
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

type TestResult = Result<(), String>;

/// The contract budget for a graceful stop (`podman stop` grace period).
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

fn binary() -> PathBuf {
    std::env::var_os("NEXTEST_BIN_EXE_onshape-mcp")
        .filter(|path| !path.is_empty())
        .map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_onshape-mcp")),
            PathBuf::from,
        )
}

fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}

/// HTTP transport mode of the server under test.
#[derive(Clone, Copy)]
enum Mode {
    ExternalToken,
    OAuth,
}

/// A running HTTP server with an isolated environment.
struct Server {
    child: Child,
    port: u16,
    _isolation: tempfile::TempDir,
}

impl Server {
    fn start(mode: Mode) -> Result<Self, String> {
        let isolation = tempfile::tempdir().map_err(|e| e.to_string())?;
        let port = free_port()?;
        let mut cmd = Command::new(binary());
        for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("ONSHAPE_MCP_")) {
            cmd.env_remove(key);
        }
        cmd.env("XDG_CONFIG_HOME", isolation.path().join("config"))
            .env("XDG_DATA_HOME", isolation.path().join("data"))
            .env("HOME", isolation.path().join("home"))
            .env("ONSHAPE_MCP_HTTP__HOST", "127.0.0.1")
            .env("ONSHAPE_MCP_HTTP__PORT", port.to_string())
            .env(
                "ONSHAPE_MCP_HTTP__PUBLIC_URL",
                format!("http://127.0.0.1:{port}/"),
            )
            .arg("http")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        match mode {
            Mode::ExternalToken => cmd
                .env(
                    "ONSHAPE_MCP_EXTERNAL_TOKEN__FILE",
                    isolation.path().join("missing-token.json"),
                )
                .env(
                    "ONSHAPE_MCP_EXTERNAL_TOKEN__DOCUMENT_ID",
                    "aaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .env(
                    "ONSHAPE_MCP_EXTERNAL_TOKEN__WORKSPACE_ID",
                    "bbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .env(
                    "ONSHAPE_MCP_EXTERNAL_TOKEN__ELEMENT_ID",
                    "cccccccccccccccccccccccc",
                ),
            Mode::OAuth => cmd
                .env("ONSHAPE_MCP_HTTP__ONSHAPE_CLIENT_ID", "fake-test-client-id")
                .env(
                    "ONSHAPE_MCP_HTTP__ONSHAPE_CLIENT_SECRET",
                    "fake-test-client-secret",
                ),
        };
        let child = cmd.spawn().map_err(|e| e.to_string())?;
        let mut server = Self {
            child,
            port,
            _isolation: isolation,
        };
        server.wait_live()?;
        Ok(server)
    }

    fn wait_live(&mut self) -> TestResult {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("server exited early: {status}"));
            }
            if health_status(self.port).as_deref() == Some("HTTP/1.1 200 OK") {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("server did not become live".to_owned())
    }

    fn signal(&self, name: &str) -> TestResult {
        let status = Command::new("kill")
            .args(["-s", name, &self.child.id().to_string()])
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill -s {name} failed: {status}"))
        }
    }

    /// Wait up to `limit`; kill the child and fail if it is still running.
    fn wait_exit(&mut self, limit: Duration) -> Result<(ExitStatus, Duration), String> {
        let started = Instant::now();
        while started.elapsed() < limit {
            if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                return Ok((status, started.elapsed()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Err(format!("server still running after {limit:?}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn health_status(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    response.lines().next().map(str::to_owned)
}

fn assert_clean_exit(server: &mut Server, signal: &str) -> TestResult {
    server.signal(signal)?;
    let (status, elapsed) = server.wait_exit(SHUTDOWN_BUDGET)?;
    eprintln!("{signal}: exit {status} after {elapsed:?}");
    assert_eq!(status.code(), Some(0), "{signal}: {status}");
    Ok(())
}

#[test]
fn sigterm_stops_server_with_exit_zero() -> TestResult {
    let mut server = Server::start(Mode::ExternalToken)?;
    assert_clean_exit(&mut server, "TERM")
}

#[test]
fn sigterm_stops_oauth_http_server_with_exit_zero() -> TestResult {
    let mut server = Server::start(Mode::OAuth)?;
    assert_clean_exit(&mut server, "TERM")
}

#[test]
fn sigint_stops_server_with_exit_zero() -> TestResult {
    let mut server = Server::start(Mode::ExternalToken)?;
    assert_clean_exit(&mut server, "INT")
}

#[test]
fn sigterm_with_stalled_in_flight_request_stops_within_budget() -> TestResult {
    let mut server = Server::start(Mode::ExternalToken)?;
    // An in-flight request whose body never arrives keeps a connection busy.
    let mut stalled = TcpStream::connect(("127.0.0.1", server.port)).map_err(|e| e.to_string())?;
    stalled
        .write_all(
            b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
              Accept: application/json, text/event-stream\r\nContent-Length: 100000\r\n\r\n{",
        )
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(200));
    let result = assert_clean_exit(&mut server, "TERM");
    drop(stalled);
    result
}

#[test]
fn healthcheck_probes_live_external_token_server() -> TestResult {
    let mut server = Server::start(Mode::ExternalToken)?;
    let probe = |path: &str| {
        Command::new(binary())
            .args(["healthcheck", "--url"])
            .arg(format!("http://127.0.0.1:{}{path}", server.port))
            .output()
            .map_err(|e| e.to_string())
    };
    let live = probe("/health")?;
    assert_eq!(live.status.code(), Some(0), "{live:?}");
    assert!(live.stderr.is_empty(), "{live:?}");
    // The token file is missing, so the server is live but not ready.
    let ready = probe("/health/ready")?;
    assert_eq!(ready.status.code(), Some(1), "{ready:?}");
    assert_eq!(
        String::from_utf8_lossy(&ready.stderr),
        "healthcheck: unhealthy status 503\n"
    );
    assert_clean_exit(&mut server, "TERM")
}
