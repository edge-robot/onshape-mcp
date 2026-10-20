//! Integration tests for `onshape-mcp healthcheck` against local listeners.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type TestResult = Result<(), String>;

fn binary() -> PathBuf {
    std::env::var_os("NEXTEST_BIN_EXE_onshape-mcp")
        .filter(|path| !path.is_empty())
        .map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_onshape-mcp")),
            PathBuf::from,
        )
}

/// Run the binary with an isolated environment and no config available.
fn healthcheck(args: &[&str]) -> Result<Output, String> {
    let isolation = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(binary());
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("ONSHAPE_MCP_")) {
        cmd.env_remove(key);
    }
    cmd.env("XDG_CONFIG_HOME", isolation.path().join("config"))
        .env("XDG_DATA_HOME", isolation.path().join("data"))
        .env("HOME", isolation.path().join("home"))
        .arg("healthcheck")
        .args(args);
    cmd.output().map_err(|e| e.to_string())
}

/// Serve one connection: record the request line, answer with `response`.
fn serve_once(response: impl Into<String>) -> Result<(u16, JoinHandle<String>), String> {
    let response: String = response.into();
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let handle = std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return String::new();
        };
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        let _ = reader.read_line(&mut request_line);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line == "\r\n" => break,
                Ok(_) => {}
            }
        }
        let _ = reader.get_mut().write_all(response.as_bytes());
        request_line
    });
    Ok((port, handle))
}

fn stderr_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn http_200_exits_zero_silently_without_config() -> TestResult {
    let (port, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")?;
    let url = format!("http://127.0.0.1:{port}/health");
    // A nonexistent --config must not matter: the probe never loads config.
    let output = Command::new(binary())
        .args([
            "--config",
            "/nonexistent/config.toml",
            "healthcheck",
            "--url",
            &url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let request_line = server.join().map_err(|_| "server panicked")?;
    assert_eq!(request_line, "GET /health HTTP/1.1\r\n");
    Ok(())
}

#[test]
fn http_503_exits_one_with_single_status_line() -> TestResult {
    let (port, server) = serve_once(
        "HTTP/1.1 503 Service Unavailable\r\nX-Leak: header-marker\r\nContent-Length: 11\r\n\r\nbody-marker",
    )?;
    let url = format!("http://127.0.0.1:{port}/health");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: unhealthy status 503"]);
    server.join().map_err(|_| "server panicked")?;
    Ok(())
}

#[test]
fn non_200_success_codes_are_unhealthy() -> TestResult {
    let (port, server) = serve_once("HTTP/1.1 204 No Content\r\n\r\n")?;
    let url = format!("http://localhost:{port}/health");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: unhealthy status 204"]);
    server.join().map_err(|_| "server panicked")?;
    Ok(())
}

#[test]
fn garbage_response_exits_one() -> TestResult {
    let (port, server) = serve_once("SSH-2.0-not-http\r\n")?;
    let url = format!("http://127.0.0.1:{port}/");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: invalid response"]);
    server.join().map_err(|_| "server panicked")?;
    Ok(())
}

#[test]
fn closed_port_exits_one() -> TestResult {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        listener.local_addr().map_err(|e| e.to_string())?.port()
    };
    let url = format!("http://127.0.0.1:{port}/health");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: connection refused"]);
    Ok(())
}

#[test]
fn silent_listener_times_out_within_budget() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // Accept and hold the connection open without ever answering.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let held: Option<TcpStream> = listener.accept().ok().map(|(s, _)| s);
        let _ = done_rx.recv_timeout(Duration::from_secs(10));
        drop(held);
    });
    let url = format!("http://127.0.0.1:{port}/health");
    let started = Instant::now();
    let output = healthcheck(&["--url", &url, "--timeout-ms", "300"])?;
    let elapsed = started.elapsed();
    let _ = done_tx.send(());
    holder.join().map_err(|_| "holder panicked")?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: timeout"]);
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    Ok(())
}

#[test]
fn ready_path_is_requested_verbatim() -> TestResult {
    // `--ready` only switches the default URL (port 8080), which a test cannot
    // own; `ready_switches_default_path` covers that mapping. Here the ready URL
    // is probed explicitly and the request line is checked.
    let (port, server) = serve_once("HTTP/1.1 200 OK\r\n\r\n")?;
    let url = format!("http://127.0.0.1:{port}/health/ready");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let request_line = server.join().map_err(|_| "server panicked")?;
    assert_eq!(request_line, "GET /health/ready HTTP/1.1\r\n");
    Ok(())
}

#[test]
fn ready_flag_conflicts_with_url() -> TestResult {
    let output = healthcheck(&["--ready", "--url", "http://127.0.0.1:1/health"])?;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    Ok(())
}

#[test]
fn non_loopback_url_exits_two_without_connecting() -> TestResult {
    for url in [
        "http://10.255.255.1:8080/health",
        "http://clawonshape-mcp:8080/health",
        "https://127.0.0.1:8080/health",
    ] {
        let output = healthcheck(&["--url", url])?;
        assert_eq!(output.status.code(), Some(2), "{url}: {output:?}");
        let lines = stderr_lines(&output);
        assert_eq!(lines.len(), 1, "{url}: {lines:?}");
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with("healthcheck: invalid argument: ")),
            "{url}: {lines:?}"
        );
    }
    Ok(())
}

#[test]
fn zero_timeout_exits_two() -> TestResult {
    let output = healthcheck(&["--timeout-ms", "0"])?;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        stderr_lines(&output),
        ["healthcheck: invalid argument: timeout-ms must be greater than 0"]
    );
    Ok(())
}

#[test]
fn closed_without_status_line_exits_one() -> TestResult {
    let (port, server) = serve_once("")?;
    let url = format!("http://127.0.0.1:{port}/health");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: invalid response"]);
    server.join().map_err(|_| "server panicked")?;
    Ok(())
}

#[test]
fn overlong_status_line_exits_one() -> TestResult {
    let (port, server) = serve_once(format!("HTTP/1.1 200 {}", "x".repeat(4096)))?;
    let url = format!("http://127.0.0.1:{port}/health");
    let output = healthcheck(&["--url", &url])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stderr_lines(&output), ["healthcheck: invalid response"]);
    server.join().map_err(|_| "server panicked")?;
    Ok(())
}
