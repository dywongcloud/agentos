use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use holoiroh_daemon::holo_bridge::listener::{
    CHILD_LISTEN_FD, HoloA2aListener, inherit_listener, inherited_holo_command,
};
use tokio::process::Child;

const CYCLES: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN: &str = "holo-fd-restart-probe-token";

#[tokio::main]
async fn main() -> Result<()> {
    verify_source_equals_destination().await?;
    let holo_bin = std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|home| format!("{home}/.holo/bin/holo"))
            .unwrap_or_else(|_| "holo".to_string())
    });
    let runtime_port = reserve_runtime_port()?;
    let _runtime_cleanup = RuntimeCleanup(runtime_port);
    let listener = HoloA2aListener::bind(0)?;
    let stable_address = listener.address();
    let mut child = spawn_child(&holo_bin, &listener, runtime_port, true)?;
    queued_health(stable_address).await?;

    for cycle in 1..=CYCLES {
        ensure!(
            listener.address() == stable_address,
            "parent address changed"
        );
        let old_connection = accepted_long_lived_request(stable_address)?;
        sigkill(&child)?;
        let output = tokio::time::timeout(REAP_TIMEOUT, child.wait_with_output())
            .await
            .context("timed out reaping SIGKILLed child")??;
        reject_bind_error(&output.stderr)?;
        require_old_connection_closed(old_connection)?;

        child = spawn_child(&holo_bin, &listener, runtime_port, false)?;
        queued_health(stable_address).await?;
        fresh_agent_card(stable_address).await?;
        ensure!(
            listener.address() == stable_address,
            "address changed after restart"
        );
        println!(
            "cycle {cycle}/8: spawn_attempts=1 address={stable_address} old_connection=closed fresh_health=ok fresh_request=ok eaddrinuse=absent"
        );
    }

    sigkill(&child)?;
    let output = tokio::time::timeout(REAP_TIMEOUT, child.wait_with_output())
        .await
        .context("timed out reaping final child")??;
    reject_bind_error(&output.stderr)?;
    let fixed_flags = unsafe { libc::fcntl(CHILD_LISTEN_FD, libc::F_GETFD) };
    if listener.raw_fd() == CHILD_LISTEN_FD {
        ensure!(
            fixed_flags & libc::FD_CLOEXEC != 0,
            "parent listener lost CLOEXEC"
        );
    } else {
        ensure!(fixed_flags == -1, "parent leaked fixed child descriptor");
    }
    println!(
        "holo_fd_restart_probe: PASS cycles=8 stable_address={stable_address} parent_listeners=1 parent_child_fd_leak=absent"
    );
    Ok(())
}

async fn verify_source_equals_destination() -> Result<()> {
    let existing = unsafe { libc::fcntl(CHILD_LISTEN_FD, libc::F_GETFD) };
    ensure!(
        existing == -1,
        "probe process already uses child fd {CHILD_LISTEN_FD}"
    );
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let duplicated = unsafe { libc::dup2(listener.as_raw_fd(), CHILD_LISTEN_FD) };
    if duplicated == -1 {
        return Err(std::io::Error::last_os_error()).context("failed to reserve fixed child fd");
    }
    let fixed = unsafe { OwnedFd::from_raw_fd(duplicated) };
    let flags = unsafe { libc::fcntl(fixed.as_raw_fd(), libc::F_GETFD) };
    ensure!(flags != -1);
    ensure!(
        unsafe { libc::fcntl(fixed.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC,) } != -1
    );

    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("test -e /dev/fd/{CHILD_LISTEN_FD}"));
    inherit_listener(&mut command, fixed.as_raw_fd());
    let status = command.status().await?;
    ensure!(
        status.success(),
        "source==destination did not clear FD_CLOEXEC"
    );
    drop(fixed);
    println!("source==destination: fd={CHILD_LISTEN_FD} cloexec=cleared-in-child");
    Ok(())
}

struct RuntimeCleanup(u16);

impl Drop for RuntimeCleanup {
    fn drop(&mut self) {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let pid_path = std::path::PathBuf::from(home)
            .join(".holo")
            .join(format!("agent-pid-{}", self.0));
        let Ok(value) = std::fs::read_to_string(&pid_path) else {
            return;
        };
        if let Ok(pid) = value.trim().parse::<libc::pid_t>() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(pid_path);
    }
}

fn reserve_runtime_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn spawn_child(
    holo_bin: &str,
    listener: &HoloA2aListener,
    runtime_port: u16,
    fake: bool,
) -> Result<Child> {
    let mut command = inherited_holo_command(holo_bin, listener)?;
    command
        .arg("--port")
        .arg(listener.port().to_string())
        .env("HOLO_AUTH_TOKEN", TOKEN)
        .env("HAI_AGENT_RUNTIME_PORT", runtime_port.to_string());
    if fake {
        command.arg("--fake");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.spawn().context("first child spawn attempt failed")
}

fn sigkill(child: &Child) -> Result<()> {
    let pid = child.id().context("child has no PID before SIGKILL")?;
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    if result == -1 {
        return Err(std::io::Error::last_os_error()).context("SIGKILL failed");
    }
    Ok(())
}

async fn queued_health(address: SocketAddr) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(address).context("queued health connect failed")?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
        let response = read_http_response(&mut stream)?;
        let text = String::from_utf8(response).context("health response was not UTF-8")?;
        ensure!(
            text.starts_with("HTTP/1.1 200"),
            "health was not HTTP 200: {text}"
        );
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .context("health response had no body")?;
        let health: serde_json::Value = serde_json::from_str(body)?;
        ensure!(health["service"] == "holo-desktop");
        ensure!(health["status"] == "ok");
        ensure!(
            health["version"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

fn accepted_long_lived_request(address: SocketAddr) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(REAP_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n")?;
    let response = read_http_response(&mut stream)?;
    ensure!(response.starts_with(b"HTTP/1.1 200"));
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n")?;
    Ok(stream)
}

fn read_http_response(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let header_end = loop {
        if let Some(index) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break index + 4;
        }
        let mut buffer = [0_u8; 1024];
        let count = stream.read(&mut buffer)?;
        ensure!(count > 0, "connection closed before HTTP headers");
        response.extend_from_slice(&buffer[..count]);
        ensure!(
            response.len() <= 64 * 1024,
            "HTTP headers exceeded probe bound"
        );
    };
    let headers = std::str::from_utf8(&response[..header_end])?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.strip_prefix("content-length: ")
                .or_else(|| line.strip_prefix("Content-Length: "))
        })
        .context("response had no Content-Length")?
        .parse::<usize>()?;
    let expected = header_end + content_length;
    while response.len() < expected {
        let mut buffer = [0_u8; 1024];
        let count = stream.read(&mut buffer)?;
        ensure!(count > 0, "connection closed before HTTP body");
        response.extend_from_slice(&buffer[..count]);
    }
    Ok(response)
}

fn require_old_connection_closed(mut stream: TcpStream) -> Result<()> {
    let mut byte = [0_u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            Ok(())
        }
        Ok(count) => bail!("old connection produced {count} byte(s) after child reap"),
        Err(error) => Err(error).context("old accepted connection did not close after child reap"),
    }
}

async fn fresh_agent_card(address: SocketAddr) -> Result<()> {
    let response = reqwest::Client::builder()
        .timeout(IO_TIMEOUT)
        .build()?
        .get(format!("http://{address}/.well-known/agent-card.json"))
        .bearer_auth(TOKEN)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "agent card status was {}",
        response.status()
    );
    let card: serde_json::Value = response.json().await?;
    ensure!(card.is_object(), "agent card was not a JSON object");
    Ok(())
}

fn reject_bind_error(stderr: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(stderr);
    let lowercase = text.to_ascii_lowercase();
    ensure!(
        !lowercase.contains("eaddrinuse"),
        "child emitted EADDRINUSE: {text}"
    );
    ensure!(
        !lowercase.contains("address already in use"),
        "child emitted address-in-use: {text}"
    );
    ensure!(
        !lowercase.contains("errno 48"),
        "child emitted Errno 48: {text}"
    );
    Ok(())
}
