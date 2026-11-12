#!/usr/bin/env python3
"""Provision one physically connected Bot for the authenticated local WSS gateway.

Quit the Server app and disconnect the USB debug window before running. Reopen
the app afterwards so its in-memory credential store reloads the new token.
Only a token hash is persisted on the Mac; the token is sent over USB and is
not printed. The current board has not enabled NVS/flash encryption.
"""
from __future__ import annotations
import argparse
import base64
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import select
import ssl
import struct
import subprocess
import sys
import termios
import time
import tty
import uuid

APP = Path(__file__).resolve().parents[1]


def atomic_json(path: Path, value) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temporary = path.with_name("." + path.name + ".new-" + secrets.token_hex(6))
    try:
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as output:
            json.dump(value, output, ensure_ascii=False, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def send_usb(port: str, payload: bytes) -> None:
    descriptor = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        fcntl.ioctl(descriptor, termios.TIOCEXCL)
        settings = termios.tcgetattr(descriptor)
        tty.setraw(descriptor)
        settings = termios.tcgetattr(descriptor)
        settings[4] = settings[5] = termios.B115200
        settings[2] |= termios.CLOCAL | termios.CREAD
        settings[2] &= ~getattr(termios, "CRTSCTS", 0)
        termios.tcsetattr(descriptor, termios.TCSANOW, settings)
        fcntl.ioctl(descriptor, termios.TIOCMBIC, struct.pack("I", termios.TIOCM_DTR | termios.TIOCM_RTS))
        termios.tcflush(descriptor, termios.TCIFLUSH)
        # Opening the physical port may reset the board. Wait until the local
        # runtime or USB command task is alive before sending the credential.
        startup = bytearray()
        ready_deadline = time.monotonic() + 25
        while time.monotonic() < ready_deadline:
            if select.select([descriptor], [], [], 0.3)[0]:
                try:
                    startup.extend(os.read(descriptor, 8192))
                except BlockingIOError:
                    pass
                if b"Local USB bring-up control enabled" in startup or b"mode=" in startup:
                    break
                if len(startup) > 65536: del startup[:-32768]
        else:
            raise RuntimeError("Bot did not reach its local runtime; no credential was sent")
        # Keep USB reset/control lines unchanged; send one bounded command.
        command = b"secure config " + base64.b64encode(payload) + b"\n"
        if len(command) >= 4096:
            raise RuntimeError("Provisioning message is too large")
        deadline = time.monotonic() + 30
        # Clear any partial command left by an interrupted previous connection.
        os.write(descriptor, b"\n")
        time.sleep(0.1)
        view = memoryview(command)
        while view and time.monotonic() < deadline:
            if select.select([], [descriptor], [], 0.5)[1]:
                view = view[os.write(descriptor, view[:16]):]
                # The board shares a modest UART input buffer with its real-time
                # audio/display tasks; a single large host write can overflow it.
                time.sleep(0.02)
        if view:
            raise RuntimeError("USB provisioning write timed out")
        # Never print arbitrary serial input: it may contain private logs.
        received = bytearray()
        while time.monotonic() < deadline:
            if not select.select([descriptor], [], [], 0.5)[0]:
                continue
            try:
                received.extend(os.read(descriptor, 8192))
            except BlockingIOError:
                continue
            if b"XIAODAI_PROVISION_OK" in received or b"USB secure provisioning saved; restarting" in received:
                return
            if b"USB secure provisioning failed" in received:
                raise RuntimeError("Bot rejected the provisioning configuration")
            if len(received) > 65536:
                del received[:-32768]
        raise RuntimeError("Bot did not acknowledge provisioning; no Mac credential was changed")
    finally:
        os.close(descriptor)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.home() / "小呆 Server")
    parser.add_argument("--port", required=True)
    parser.add_argument("--device-id", required=True)
    parser.add_argument("--hostname", help="Optional TLS identity (defaults to the certificate or this Server's stable Bonjour name)")
    parser.add_argument("--ip", help="Optional extra IPv4 SAN when creating a certificate; discovery does not require it")
    parser.add_argument("--tls-directory", type=Path, help="TLS material directory inside the Server root (default: tls)")
    args = parser.parse_args()
    root = args.root.expanduser().resolve()
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", args.device_id):
        parser.error("Invalid device ID")
    if args.hostname and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]{0,251}[A-Za-z0-9]", args.hostname):
        parser.error("Invalid hostname")
    if args.ip:
        try:
            address=ipaddress.IPv4Address(args.ip)
            if address.is_unspecified or address.is_multicast or address.is_loopback: raise ValueError()
        except ValueError:
            parser.error("Expected a LAN IPv4 address for the extra SAN")
    if not (root / "brain.json").is_file() or (root / ".migration-in-progress").exists():
        raise RuntimeError("Server data migration must complete first")
    tls = (args.tls_directory or root / "tls").expanduser().resolve()
    if not tls.is_relative_to(root):
        parser.error("TLS material must be inside the Server root")
    certificate, key = tls / "gateway-cert.pem", tls / "gateway-key.pem"
    ca_certificate, ca_key = tls / "gateway-ca.pem", tls / "gateway-ca-key.pem"
    if not any(path.exists() for path in (certificate, key, ca_certificate, ca_key)):
        if not args.hostname:
            id_path=root / "server_id.txt"
            try:
                with id_path.open("x") as entry: entry.write(uuid.uuid4().hex + "\n")
            except FileExistsError:
                pass
            server_id=id_path.read_text().strip()
            if not re.fullmatch(r"[A-Za-z0-9_-]{8,47}",server_id):
                raise RuntimeError("Invalid saved Server identity")
            args.hostname=f"xiaodai-{server_id.lower().replace('_','-')}.local"
        cmd = [sys.executable, str(APP / "scripts/create_gateway_certificate.py"), "--directory", str(tls)]
        try:
            cmd += ["--ip",str(ipaddress.IPv4Address(args.hostname))]
        except ipaddress.AddressValueError:
            cmd += ["--dns", args.hostname]
        if args.ip and args.ip != args.hostname: cmd += ["--ip", args.ip]
        subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)
    if not all(path.is_file() for path in (certificate, key, ca_certificate, ca_key)):
        raise RuntimeError("Incomplete or legacy TLS material; use a new --tls-directory for rotation")
    # Check hostname before touching the device or replacing any credential.
    try:
        names=ssl._ssl._test_decode_cert(str(certificate)).get("subjectAltName", ())
    except (ssl.SSLError, OSError) as error:
        raise RuntimeError("Cannot read the existing gateway certificate") from error
    if not args.hostname:
        args.hostname=next((name for kind,name in names if kind=="DNS" and "*" not in name),None)
        if not args.hostname: args.hostname=next((name for kind,name in names if kind=="IP Address" and ":" not in name),None)
    if not args.hostname or not any(kind in ("DNS","IP Address") and name.lower()==args.hostname.lower() for kind,name in names):
        raise RuntimeError("Existing certificate does not contain the requested host or IP; rotate explicitly")
    token = secrets.token_hex(32)
    payload = json.dumps({"url":f"wss://{args.hostname}:7443/v3/brain/ws", "certificate":ca_certificate.read_text(), "token":token},
                         separators=(",", ":")).encode()
    send_usb(args.port, payload)
    path = root / "runtime/devices/secure-devices.json"
    records = json.loads(path.read_text()) if path.exists() else {}
    records[args.device_id] = {"token_sha256":hashlib.sha256(token.encode()).hexdigest(), "created_at_ms":int(time.time()*1000)}
    atomic_json(path, records)
    config_path = root / "brain.json"
    config = json.loads(config_path.read_text())
    config["gateway"] = {**config.get("gateway", {}), "enabled":True, "bind":"0.0.0.0:7443", "pairing_ca_pem":str(ca_certificate),
                         "certificate_pem":str(certificate), "private_key_pem":str(key)}
    config["auto_start"] = True
    atomic_json(config_path, config)
    print("USB provisioning acknowledged; gateway enabled. Reopen 小呆 Server to load the new credential.")


if __name__ == "__main__":
    main()
