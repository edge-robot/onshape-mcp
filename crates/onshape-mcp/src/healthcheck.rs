//! `onshape-mcp healthcheck`: a loopback-only HTTP probe for distroless images.
//!
//! The probe sends one HTTP/1.1 `GET` over a blocking [`std::net::TcpStream`] and
//! parses only the status line. It starts no async runtime, reads no config,
//! token or approval file, and prints nothing on success. On failure it prints
//! exactly one line naming the status or the error class, never a response body,
//! a header or an environment value.

use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Default liveness URL probed when `--url` is omitted.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8080/health";
/// Default readiness URL probed with `--ready`.
pub const DEFAULT_READY_URL: &str = "http://127.0.0.1:8080/health/ready";
/// Default probe timeout in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 3000;
/// Upper bound for `--timeout-ms`; larger values are capped to this.
pub const MAX_TIMEOUT_MS: u64 = 10_000;

/// Exit code for HTTP 200.
pub const EXIT_HEALTHY: i32 = 0;
/// Exit code for any other status, a connection error or a timeout.
pub const EXIT_UNHEALTHY: i32 = 1;
/// Exit code for invalid arguments.
pub const EXIT_INVALID_ARGS: i32 = 2;

/// Upper bound on bytes read while looking for the status line.
const MAX_STATUS_LINE: usize = 1024;

/// Arguments of the `healthcheck` subcommand.
#[derive(Debug, Clone, clap::Args)]
pub struct HealthcheckArgs {
    /// URL to probe. Must be `http://` on 127.0.0.1, `[::1]` or localhost.
    ///
    /// Default: `http://127.0.0.1:8080/health`.
    #[arg(long, conflicts_with = "ready")]
    pub url: Option<String>,

    /// Probe `/health/ready` instead of `/health` on the default address.
    #[arg(long)]
    pub ready: bool,

    /// Total timeout in milliseconds (connect, write and read); capped at 10000.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_MS)]
    pub timeout_ms: u64,
}

/// Rejected `healthcheck` arguments (exit code 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgError {
    /// The URL does not start with `http://`.
    Scheme,
    /// The host is not 127.0.0.1, `::1` or localhost.
    NonLoopbackHost,
    /// The port is missing after `:`, not a number, or zero.
    Port,
    /// User info, a fragment, whitespace or a non-printable character.
    Malformed,
    /// `--timeout-ms 0`.
    ZeroTimeout,
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Scheme => "url scheme must be http",
            Self::NonLoopbackHost => "url host must be loopback (127.0.0.1, [::1] or localhost)",
            Self::Port => "url port is invalid",
            Self::Malformed => "url is malformed",
            Self::ZeroTimeout => "timeout-ms must be greater than 0",
        })
    }
}

impl std::error::Error for ArgError {}

/// Why a well-formed probe did not observe HTTP 200 (exit code 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The server answered with a status other than 200.
    Status(u16),
    /// Nothing is listening on the port.
    ConnectionRefused,
    /// The connection could not be established or broke for another reason.
    ConnectionFailed,
    /// The deadline expired before a status line arrived.
    Timeout,
    /// The response did not start with a valid HTTP/1.x status line.
    InvalidResponse,
}

impl std::error::Error for ProbeFailure {}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(code) => write!(f, "unhealthy status {code}"),
            Self::ConnectionRefused => f.write_str("connection refused"),
            Self::ConnectionFailed => f.write_str("connection failed"),
            Self::Timeout => f.write_str("timeout"),
            Self::InvalidResponse => f.write_str("invalid response"),
        }
    }
}

/// A validated probe target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Loopback socket addresses, tried in order.
    addrs: Vec<SocketAddr>,
    /// Value of the `Host` header (`host[:port]` as given).
    authority: String,
    /// Request target, starting with `/`.
    path: String,
    /// Total time budget.
    timeout: Duration,
}

impl Probe {
    /// Validate the arguments and build a probe; never touches the network.
    ///
    /// # Errors
    /// [`ArgError`] for a non-`http://` URL, a non-loopback host, a bad port,
    /// a malformed URL or a zero timeout.
    pub fn from_args(args: &HealthcheckArgs) -> Result<Self, ArgError> {
        let url = match (&args.url, args.ready) {
            (Some(url), _) => url.as_str(),
            (None, true) => DEFAULT_READY_URL,
            (None, false) => DEFAULT_URL,
        };
        if args.timeout_ms == 0 {
            return Err(ArgError::ZeroTimeout);
        }
        let timeout = Duration::from_millis(args.timeout_ms.min(MAX_TIMEOUT_MS));
        let (addrs, authority, path) = parse_url(url)?;
        Ok(Self {
            addrs,
            authority,
            path,
            timeout,
        })
    }

    /// The effective (capped) timeout.
    #[cfg(test)]
    const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Send one `GET` and return `Ok(())` only for HTTP 200.
    ///
    /// # Errors
    /// [`ProbeFailure`] for any other status, a connection error or a timeout.
    pub fn run(&self) -> Result<(), ProbeFailure> {
        let deadline = Instant::now() + self.timeout;
        let mut stream = self.connect(deadline)?;
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: onshape-mcp-healthcheck\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            self.path, self.authority
        );
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|_| ProbeFailure::ConnectionFailed)?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| io_failure(&error))?;
        let line = read_status_line(&mut stream, deadline)?;
        match parse_status_line(&line) {
            Some(200) => Ok(()),
            Some(code) => Err(ProbeFailure::Status(code)),
            None => Err(ProbeFailure::InvalidResponse),
        }
    }

    fn connect(&self, deadline: Instant) -> Result<TcpStream, ProbeFailure> {
        let mut last = ProbeFailure::ConnectionFailed;
        for addr in &self.addrs {
            match TcpStream::connect_timeout(addr, remaining(deadline)?) {
                Ok(stream) => return Ok(stream),
                Err(error) => last = io_failure(&error),
            }
        }
        Err(last)
    }
}

/// Run the subcommand and return the process exit code.
pub fn run(args: &HealthcheckArgs) -> i32 {
    let probe = match Probe::from_args(args) {
        Ok(probe) => probe,
        Err(error) => {
            eprintln!("healthcheck: invalid argument: {error}");
            return EXIT_INVALID_ARGS;
        }
    };
    match probe.run() {
        Ok(()) => EXIT_HEALTHY,
        Err(failure) => {
            eprintln!("healthcheck: {failure}");
            EXIT_UNHEALTHY
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration, ProbeFailure> {
    let left = deadline.saturating_duration_since(Instant::now());
    // Zero durations are rejected by the std socket timeout setters.
    if left.is_zero() {
        Err(ProbeFailure::Timeout)
    } else {
        Ok(left)
    }
}

fn io_failure(error: &std::io::Error) -> ProbeFailure {
    match error.kind() {
        ErrorKind::ConnectionRefused => ProbeFailure::ConnectionRefused,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => ProbeFailure::Timeout,
        _ => ProbeFailure::ConnectionFailed,
    }
}

fn read_status_line(stream: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>, ProbeFailure> {
    let mut line = Vec::with_capacity(64);
    let mut buf = [0_u8; 256];
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| ProbeFailure::ConnectionFailed)?;
        let n = match stream.read(&mut buf) {
            Ok(0) => return Err(ProbeFailure::InvalidResponse),
            Ok(n) => n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_failure(&error)),
        };
        let chunk = buf.get(..n).ok_or(ProbeFailure::InvalidResponse)?;
        if let Some(end) = chunk.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(chunk.get(..end).ok_or(ProbeFailure::InvalidResponse)?);
            return Ok(line);
        }
        line.extend_from_slice(chunk);
        if line.len() > MAX_STATUS_LINE {
            return Err(ProbeFailure::InvalidResponse);
        }
    }
}

/// Parse `HTTP/1.x SSS[ reason]` and return the status code.
fn parse_status_line(line: &[u8]) -> Option<u16> {
    let line = std::str::from_utf8(line).ok()?.trim_end_matches('\r');
    let rest = line
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| line.strip_prefix("HTTP/1.0 "))?;
    let code = rest.get(..3)?;
    if !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !matches!(rest.as_bytes().get(3), None | Some(b' ')) {
        return None;
    }
    code.parse().ok().filter(|code| (100..=599).contains(code))
}

/// Split `http://host[:port][/path]` into loopback addresses, authority and path.
fn parse_url(url: &str) -> Result<(Vec<SocketAddr>, String, String), ArgError> {
    let rest = url.strip_prefix("http://").ok_or(ArgError::Scheme)?;
    // Reject anything that could split or smuggle request lines or headers.
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ArgError::Malformed);
    }
    if rest.contains(['@', '#', '\\']) {
        return Err(ArgError::Malformed);
    }
    let (authority, path) = rest
        .find(['/', '?'])
        .map_or((rest, "/"), |i| rest.split_at(i));
    let path = if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_owned()
    };
    let (host, port) = split_host_port(authority)?;
    let ips: Vec<IpAddr> = match host {
        "127.0.0.1" => vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        "[::1]" => vec![IpAddr::V6(Ipv6Addr::LOCALHOST)],
        // Resolved without DNS or /etc/hosts so the probe cannot be redirected.
        h if h.eq_ignore_ascii_case("localhost") => vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ],
        _ => return Err(ArgError::NonLoopbackHost),
    };
    let addrs = ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    Ok((addrs, authority.to_owned(), path))
}

fn split_host_port(authority: &str) -> Result<(&str, u16), ArgError> {
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(ArgError::Malformed)?;
        let (host, tail) = authority.split_at(end + 1);
        match tail {
            "" => (host, None),
            _ => (
                host,
                Some(tail.strip_prefix(':').ok_or(ArgError::Malformed)?),
            ),
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        None => 80,
        Some(digits) if digits.bytes().all(|b| b.is_ascii_digit()) => match digits.parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => return Err(ArgError::Port),
        },
        Some(_) => return Err(ArgError::Port),
    };
    Ok((host, port))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn args(url: Option<&str>, ready: bool, timeout_ms: u64) -> HealthcheckArgs {
        HealthcheckArgs {
            url: url.map(str::to_owned),
            ready,
            timeout_ms,
        }
    }

    fn probe(url: &str) -> Result<Probe, ArgError> {
        Probe::from_args(&args(Some(url), false, DEFAULT_TIMEOUT_MS))
    }

    #[test]
    fn defaults_probe_liveness_on_8080() {
        let p = Probe::from_args(&args(None, false, DEFAULT_TIMEOUT_MS)).expect("valid");
        assert_eq!(p.path, "/health");
        assert_eq!(p.authority, "127.0.0.1:8080");
        assert_eq!(p.addrs, vec![SocketAddr::from(([127, 0, 0, 1], 8080))]);
        assert_eq!(p.timeout(), Duration::from_millis(3000));
    }

    #[test]
    fn ready_switches_default_path() {
        let p = Probe::from_args(&args(None, true, DEFAULT_TIMEOUT_MS)).expect("valid");
        assert_eq!(p.path, "/health/ready");
        assert_eq!(p.authority, "127.0.0.1:8080");
    }

    #[test]
    fn non_loopback_hosts_are_rejected() {
        for url in [
            "http://10.0.0.1:8080/health",
            "http://clawonshape-mcp:8080/health",
            "http://cad.onshape.com/",
            "http://127.0.0.2/health",
            "http://0.0.0.0:8080/health",
            "http://[::2]:8080/",
            "http://[::ffff:127.0.0.1]:8080/",
            "http://localhost.example.com/",
            "http://2130706433/",
            "http://:8080/",
        ] {
            assert_eq!(probe(url), Err(ArgError::NonLoopbackHost), "{url}");
        }
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        for url in [
            "https://127.0.0.1/health",
            "HTTP://127.0.0.1/",
            "file:///etc/passwd",
            "127.0.0.1:8080",
        ] {
            assert_eq!(probe(url), Err(ArgError::Scheme), "{url}");
        }
    }

    #[test]
    fn malformed_urls_are_rejected() {
        for url in [
            "http://",
            "http://user@127.0.0.1/",
            "http://127.0.0.1/a b",
            "http://127.0.0.1/\r\nX-Evil: 1",
            "http://127.0.0.1/#frag",
            "http://[::1/",
            "http://[::1]x/",
            "http://127.0.0.1\\@evil/",
        ] {
            assert_eq!(probe(url), Err(ArgError::Malformed), "{url:?}");
        }
    }

    #[test]
    fn bad_ports_are_rejected() {
        for url in [
            "http://127.0.0.1:/",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:65536/",
            "http://127.0.0.1:+80/",
            "http://localhost:x/",
        ] {
            assert_eq!(probe(url), Err(ArgError::Port), "{url}");
        }
    }

    #[test]
    fn loopback_hosts_are_accepted() {
        let p = probe("http://[::1]:9000/health/ready").expect("valid");
        assert_eq!(p.addrs, vec![SocketAddr::from((Ipv6Addr::LOCALHOST, 9000))]);
        assert_eq!(p.authority, "[::1]:9000");
        assert_eq!(p.path, "/health/ready");

        let p = probe("http://LocalHost").expect("valid");
        assert_eq!(
            p.addrs,
            vec![
                SocketAddr::from((Ipv4Addr::LOCALHOST, 80)),
                SocketAddr::from((Ipv6Addr::LOCALHOST, 80)),
            ]
        );
        assert_eq!(p.path, "/");

        let p = probe("http://127.0.0.1:1?x=1").expect("valid");
        assert_eq!(p.authority, "127.0.0.1:1");
        assert_eq!(p.path, "/?x=1");
    }

    #[test]
    fn timeout_is_capped_at_ten_seconds() {
        for (given, effective) in [
            (1, 1),
            (10_000, 10_000),
            (10_001, 10_000),
            (u64::MAX, 10_000),
        ] {
            let p = Probe::from_args(&args(None, false, given)).expect("valid");
            assert_eq!(p.timeout(), Duration::from_millis(effective), "{given}");
        }
    }

    #[test]
    fn zero_timeout_is_rejected() {
        assert_eq!(
            Probe::from_args(&args(None, false, 0)),
            Err(ArgError::ZeroTimeout)
        );
    }

    #[test]
    fn invalid_arguments_exit_two() {
        assert_eq!(
            run(&args(Some("http://10.0.0.1/"), false, 1)),
            EXIT_INVALID_ARGS
        );
        assert_eq!(run(&args(None, false, 0)), EXIT_INVALID_ARGS);
    }

    #[test]
    fn status_lines_parse_strictly() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200 OK\r"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/1.0 503"), Some(503));
        assert_eq!(parse_status_line(b"HTTP/1.1 200"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/1.1 2000 OK"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 20 OK"), None);
        assert_eq!(parse_status_line(b"HTTP/2 200 OK"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 099 x"), None);
        assert_eq!(parse_status_line(b"SSH-2.0-OpenSSH"), None);
        assert_eq!(parse_status_line(&[0xff, 0xfe]), None);
    }

    #[test]
    fn failure_lines_name_only_the_class() {
        assert_eq!(
            ProbeFailure::Status(503).to_string(),
            "unhealthy status 503"
        );
        assert_eq!(
            ProbeFailure::ConnectionRefused.to_string(),
            "connection refused"
        );
        assert_eq!(
            ProbeFailure::ConnectionFailed.to_string(),
            "connection failed"
        );
        assert_eq!(ProbeFailure::Timeout.to_string(), "timeout");
        assert_eq!(
            ProbeFailure::InvalidResponse.to_string(),
            "invalid response"
        );
    }
}
