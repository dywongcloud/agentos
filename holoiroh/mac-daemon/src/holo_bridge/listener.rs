use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read};
use std::mem::size_of;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

pub const CHILD_LISTEN_FD: RawFd = 198;
pub const HOLO_A2A_LISTEN_FD_ENV: &str = "HOLO_A2A_LISTEN_FD";
pub const LISTEN_BACKLOG: libc::c_int = 2048;
pub const PYTHON_SHIM: &str = include_str!("../../python/holo_fd_serve.py");

pub struct HoloA2aListener {
    inner: TcpListener,
    address: SocketAddr,
}

impl HoloA2aListener {
    pub fn bind(port: u16) -> Result<Self> {
        let raw_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if raw_fd == -1 {
            return Err(io::Error::last_os_error()).context("failed to create Holo A2A socket");
        }
        let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        set_close_on_exec(owned_fd.as_raw_fd())?;
        set_reuse_address(owned_fd.as_raw_fd())?;

        let mut bind_address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        #[cfg(target_os = "macos")]
        {
            bind_address.sin_len = size_of::<libc::sockaddr_in>() as u8;
        }
        bind_address.sin_family = libc::AF_INET as libc::sa_family_t;
        bind_address.sin_port = port.to_be();
        bind_address.sin_addr = libc::in_addr {
            s_addr: u32::from_ne_bytes(Ipv4Addr::LOCALHOST.octets()),
        };
        let bind_result = unsafe {
            libc::bind(
                owned_fd.as_raw_fd(),
                (&raw const bind_address).cast::<libc::sockaddr>(),
                size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        if bind_result == -1 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("failed to bind Holo A2A listener on 127.0.0.1:{port}"));
        }
        if unsafe { libc::listen(owned_fd.as_raw_fd(), LISTEN_BACKLOG) } == -1 {
            return Err(io::Error::last_os_error()).context("failed to listen on Holo A2A socket");
        }

        let inner = TcpListener::from(owned_fd);
        inner
            .set_nonblocking(true)
            .context("failed to make Holo A2A listener nonblocking")?;
        let address = inner
            .local_addr()
            .context("failed to read Holo A2A listener address")?;
        match address {
            SocketAddr::V4(value) if *value.ip() == Ipv4Addr::LOCALHOST => {}
            _ => bail!("Holo A2A listener escaped IPv4 loopback: {address}"),
        }
        if port != 0 && address.port() != port {
            bail!(
                "Holo A2A listener bound unexpected port {}; expected {port}",
                address.port()
            );
        }
        Ok(Self { inner, address })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

pub fn inherited_holo_command(holo_bin: &str, listener: &HoloA2aListener) -> Result<Command> {
    let launcher = resolve_executable(holo_bin)?;
    let (program, interpreter_args) = python_shebang(&launcher)?;
    let mut command = Command::new(program);
    command.args(interpreter_args);
    command.arg("-c").arg(PYTHON_SHIM);
    command.env(HOLO_A2A_LISTEN_FD_ENV, CHILD_LISTEN_FD.to_string());
    inherit_listener(&mut command, listener.raw_fd());
    Ok(command)
}

pub fn inherit_listener(command: &mut Command, source_fd: RawFd) {
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if source_fd == CHILD_LISTEN_FD {
                let flags = libc::fcntl(source_fd, libc::F_GETFD);
                if flags == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::fcntl(source_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            } else if libc::dup2(source_fd, CHILD_LISTEN_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn set_close_on_exec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error()).context("failed to read listener fd flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error()).context("failed to set listener FD_CLOEXEC");
    }
    Ok(())
}

fn set_reuse_address(fd: RawFd) -> Result<()> {
    let enabled: libc::c_int = 1;
    if unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&raw const enabled).cast::<libc::c_void>(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    } == -1
    {
        return Err(io::Error::last_os_error()).context("failed to set listener SO_REUSEADDR");
    }
    Ok(())
}

fn resolve_executable(program: &str) -> Result<PathBuf> {
    let direct = Path::new(program);
    if direct.components().count() > 1 {
        if direct.is_file() {
            return Ok(direct.to_path_buf());
        }
        bail!("Holo launcher does not exist: {}", direct.display());
    }

    let path = std::env::var_os("PATH").context("PATH is not set; cannot resolve Holo launcher")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("could not resolve Holo launcher {program:?} on PATH")
}

fn python_shebang(launcher: &Path) -> Result<(OsString, Vec<OsString>)> {
    let file = std::fs::File::open(launcher)
        .with_context(|| format!("failed to open Holo launcher {}", launcher.display()))?;
    let mut first_line = String::new();
    BufReader::new(file)
        .take(4096)
        .read_line(&mut first_line)
        .with_context(|| format!("failed to read Holo launcher {}", launcher.display()))?;
    let shebang = first_line
        .trim_end()
        .strip_prefix("#!")
        .with_context(|| {
            format!(
                "Holo launcher {} is not a Python shebang script; inherited-listener shim cannot use its environment",
                launcher.display()
            )
        })?;
    let mut fields = shebang.split_ascii_whitespace();
    let program = fields
        .next()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Holo launcher {} has an empty shebang", launcher.display()))?;
    let args = fields.map(OsString::from).collect();
    Ok((OsString::from(program), args))
}
