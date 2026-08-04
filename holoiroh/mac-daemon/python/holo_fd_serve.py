"""Compatibility entry point for holo-desktop-cli 0.0.2 socket activation."""

from __future__ import annotations

import errno
import hashlib
import importlib
import importlib.metadata
import inspect
import logging
import os
import re
import secrets
import socket
from typing import Annotated, Literal

import tyro

EXPECTED_HOLO_VERSION = "0.0.2"
EXPECTED_UVICORN_VERSION = "0.51.0"
EXPECTED_SOURCE_HASHES = {
    "serve": "9305fdda4811b03dd4018260dd900bd207885dbb7e9186def30859b29f09f3df",
    "build_app": "929e0580b99e2bcf60626d5a17f5f812850b01cb3b22af9df8741ece2997f4de",
    "_lifespan": "bd33e14d59acb96c868d65c2e3b755fad71e49d20171ac3677aed5eb407a1f9c",
}
LISTEN_FD_ENV = "HOLO_A2A_LISTEN_FD"
LOOPBACK_HOST = "127.0.0.1"
DARWIN_TCP_CONNECTION_INFO_SIZE = 112
DARWIN_TCPS_LISTEN = 1

LogLevel = Literal["DEBUG", "INFO", "WARNING", "ERROR"]


def _source_hash(value: object) -> str:
    return hashlib.sha256(inspect.getsource(value).encode()).hexdigest()


def require_supported_upstream() -> object:
    version = importlib.metadata.version("holo-desktop-cli")
    if version != EXPECTED_HOLO_VERSION:
        raise RuntimeError(
            f"inherited-listener shim requires holo-desktop-cli=={EXPECTED_HOLO_VERSION}; found {version}"
        )

    uvicorn_version = importlib.metadata.version("uvicorn")
    if uvicorn_version != EXPECTED_UVICORN_VERSION:
        raise RuntimeError(
            f"inherited-listener shim requires uvicorn=={EXPECTED_UVICORN_VERSION}; found {uvicorn_version}"
        )

    upstream = importlib.import_module("holo_desktop.cli.serve")
    mismatches = [
        name
        for name, expected in EXPECTED_SOURCE_HASHES.items()
        if not hasattr(upstream, name) or _source_hash(getattr(upstream, name)) != expected
    ]
    if mismatches:
        joined = ", ".join(mismatches)
        raise RuntimeError(
            f"holo-desktop-cli {version} serve API differs from the supported source: {joined}"
        )
    if upstream.LOOPBACK_HOST != LOOPBACK_HOST:
        raise RuntimeError(
            f"unsupported holo loopback host {upstream.LOOPBACK_HOST!r}; expected {LOOPBACK_HOST!r}"
        )
    return upstream


def _socket_accepting(inherited: socket.socket) -> bool:
    try:
        return inherited.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN) == 1
    except OSError as exc:
        if exc.errno != errno.ENOPROTOOPT or not hasattr(socket, "TCP_CONNECTION_INFO"):
            raise
        state = inherited.getsockopt(
            socket.IPPROTO_TCP,
            socket.TCP_CONNECTION_INFO,
            DARWIN_TCP_CONNECTION_INFO_SIZE,
        )
        return bool(state) and state[0] == DARWIN_TCPS_LISTEN


def inherited_listener_fd(expected_port: int) -> int | None:
    value = os.environ.get(LISTEN_FD_ENV)
    if value is None:
        return None
    if re.fullmatch(r"[0-9]+", value) is None:
        raise RuntimeError(f"{LISTEN_FD_ENV} must be a non-negative integer, got {value!r}")

    fd = int(value)
    try:
        with socket.fromfd(fd, socket.AF_INET, socket.SOCK_STREAM) as inherited:
            socket_type = inherited.getsockopt(socket.SOL_SOCKET, socket.SO_TYPE)
            accepting = _socket_accepting(inherited)
            address = inherited.getsockname()
    except OSError as exc:
        raise RuntimeError(f"{LISTEN_FD_ENV}={fd} is not a usable socket: {exc}") from exc

    if socket_type != socket.SOCK_STREAM:
        raise RuntimeError(f"{LISTEN_FD_ENV}={fd} is not a TCP stream socket")
    if not accepting:
        raise RuntimeError(f"{LISTEN_FD_ENV}={fd} is not listening (SO_ACCEPTCONN=false)")
    if not isinstance(address, tuple) or len(address) != 2:
        raise RuntimeError(f"{LISTEN_FD_ENV}={fd} is not an IPv4 TCP socket: {address!r}")
    host, port = address
    if host != LOOPBACK_HOST:
        raise RuntimeError(
            f"{LISTEN_FD_ENV}={fd} listens on {host!r}; expected loopback {LOOPBACK_HOST!r}"
        )
    if port != expected_port:
        raise RuntimeError(
            f"{LISTEN_FD_ENV}={fd} listens on port {port}; expected configured port {expected_port}"
        )
    return fd


def serve(
    port: int = 18794,
    model: str | None = None,
    base_url: str | None = None,
    cors_origin: Annotated[
        list[str],
        tyro.conf.UseAppendAction,
        tyro.conf.arg(metavar="ORIGIN", help="Extra CORS origin to allow. Repeatable."),
    ] = [],
    log_level: Annotated[LogLevel, tyro.conf.arg(metavar="LEVEL")] = "WARNING",
    fake: Annotated[
        bool,
        tyro.conf.arg(help="Back the server with the fake agent (no model/desktop)."),
    ] = False,
) -> None:
    upstream = require_supported_upstream()
    listen_fd = inherited_listener_fd(port)
    if listen_fd is None:
        upstream.serve(
            port=port,
            model=model,
            base_url=base_url,
            cors_origin=cors_origin,
            log_level=log_level,
            fake=fake,
        )
        return

    import uvicorn
    from rich.console import Console

    from holo_desktop.cli.bootstrap import bootstrap_interactive, ensure_guard_running

    settings = bootstrap_interactive(base_url=base_url, fake=fake)
    ensure_guard_running()
    logging.basicConfig(
        level=log_level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    base = f"http://{LOOPBACK_HOST}:{port}"
    console = Console(stderr=True)
    supplied_token = settings.serve.auth_token
    if supplied_token is not None and not supplied_token.strip():
        raise RuntimeError(f"{upstream.A2A_TOKEN_ENV} is set but empty")
    token = supplied_token or secrets.token_urlsafe(32)
    origins = tuple(cors_origin)
    console.print(f"[bold magenta]holo serve[/bold magenta] [dim]· v{upstream.__version__}[/dim]")
    console.print(f"  [cyan]{base}/a2a[/cyan]")
    if supplied_token is None:
        console.print(f"  [dim]export {upstream.A2A_TOKEN_ENV}=[/dim][yellow]{token}[/yellow]")
    console.print("  [dim]Ctrl+C to stop[/dim]")
    uvicorn.run(
        upstream.build_app(
            f"{base}/a2a",
            token,
            origins,
            model=model,
            base_url=base_url,
            fake=fake,
            settings=settings,
        ),
        host=LOOPBACK_HOST,
        port=port,
        fd=listen_fd,
        log_level=log_level.lower(),
    )


if __name__ == "__main__":
    tyro.cli(serve)
