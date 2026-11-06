#!/usr/bin/env python3
"""Explicit local TLS certificate preparation, separate from device provisioning."""
from __future__ import annotations

import argparse
import hashlib
import ipaddress
import os
from pathlib import Path
import re
import shutil
import ssl
import subprocess
import tempfile


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--dns", action="append", default=[], help="Mac's actual LAN DNS/mDNS hostname; repeat as needed")
    parser.add_argument("--ip", action="append", default=[], help="Mac LAN IP address; repeat as needed")
    args = parser.parse_args()
    if not args.dns and not args.ip:
        parser.error("At least one --dns or --ip is required for hostname verification")
    if any(not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]{0,251}[A-Za-z0-9]", host) for host in args.dns):
        parser.error("Invalid DNS name")
    try:
        ips = [str(ipaddress.ip_address(address)) for address in args.ip]
    except ValueError:
        parser.error("Invalid IP address")
    openssl = shutil.which("openssl")
    if not openssl:
        parser.error("OpenSSL is required for certificate preparation")
    directory = args.directory.expanduser().resolve()
    directory.mkdir(parents=True, exist_ok=True)
    directory.chmod(0o700)
    certificate, key = directory / "gateway-cert.pem", directory / "gateway-key.pem"
    if certificate.exists() or key.exists():
        parser.error("Existing certificate/key preserved; use a new directory for rotation")
    config = "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=extensions\n[dn]\nCN=Xiaodai Mac Bot Server\n[extensions]\nsubjectAltName=@names\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyCertSign\nextendedKeyUsage=serverAuth\n[names]\n"
    config += "".join(f"DNS.{i}={host}\n" for i, host in enumerate(args.dns, 1))
    config += "".join(f"IP.{i}={address}\n" for i, address in enumerate(ips, 1))
    old_umask = os.umask(0o077)
    try:
        with tempfile.TemporaryDirectory(prefix=".tls-", dir=directory) as temporary:
            root = Path(temporary)
            configuration = root / "openssl.cnf"
            configuration.write_text(config, encoding="ascii")
            subprocess.run([openssl, "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
                            "-nodes", "-sha256", "-days", "365", "-config", str(configuration),
                            "-out", str(root / "cert.pem"), "-keyout", str(root / "key.pem")],
                           check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
            # The private key remains only on the Mac. Trust the public cert out of band.
            os.replace(root / "key.pem", key)
            os.replace(root / "cert.pem", certificate)
    finally:
        os.umask(old_umask)
    key.chmod(0o600)
    certificate.chmod(0o600)
    der = ssl.PEM_cert_to_DER_cert(certificate.read_text(encoding="ascii"))
    print(f"Certificate: {certificate}\nPrivate key: {key}")
    print(f"Certificate SHA256: {hashlib.sha256(der).hexdigest()}")
    print("Provision the public certificate and the per-device bearer token over a trusted channel.")
    print("This script does not configure Wi-Fi, BLE, firmware or start the server.")


if __name__ == "__main__":
    main()
