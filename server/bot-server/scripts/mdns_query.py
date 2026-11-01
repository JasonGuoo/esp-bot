#!/usr/bin/env python3
"""Raw mDNS/DNS-SD browser for _espbot-server._tcp.local. (stdlib only).

Sends PTR + SRV/TXT queries to 224.0.0.251:5353 and prints what the local
mDNS stack answers — the same discovery the firmware performs, with none of
dns-sd's interactive buffering.

    python3 scripts/mdns_query.py [timeout_seconds]
"""

import socket
import struct
import sys
import time

MDNS_GROUP = ("224.0.0.251", 5353)
SERVICE = "_espbot-server._tcp.local."
PTR, TXT, SRV, A = 12, 16, 33, 1


def encode_name(name: str) -> bytes:
    return b"".join(bytes([len(part)]) + part.encode() for part in name.rstrip(".").split(".")) + b"\x00"


def build_query(questions: list[tuple[str, int]]) -> bytes:
    header = struct.pack(">HHHHHH", 0, 0, len(questions), 0, 0, 0)
    body = b"".join(encode_name(q) + struct.pack(">HH", qtype, 1) for q, qtype in questions)
    return header + body


def read_name(packet: bytes, offset: int, depth: int = 0) -> tuple[str, int]:
    if depth > 8:
        return "", offset
    labels: list[str] = []
    while True:
        if offset >= len(packet):
            return ".".join(labels) + ".", offset
        length = packet[offset]
        if length == 0:
            return ".".join(labels) + ".", offset + 1
        if length & 0xC0 == 0xC0:
            pointer = struct.unpack(">H", packet[offset : offset + 2])[0] & 0x3FFF
            pointed, _ = read_name(packet, pointer, depth + 1)
            suffix = pointed.rstrip(".").split(".") if pointed else []
            return ".".join(labels + suffix) + ".", offset + 2
        labels.append(packet[offset + 1 : offset + 1 + length].decode("utf-8", "replace"))
        offset += 1 + length


def parse_records(packet: bytes) -> list[dict]:
    qdcount, ancount = struct.unpack(">HH", packet[4:8])
    offset = 12
    for _ in range(qdcount):
        _, offset = read_name(packet, offset)
        offset += 4
    records = []
    for _ in range(ancount):
        name, offset = read_name(packet, offset)
        rtype, rclass, ttl, rdlength = struct.unpack(">HHIH", packet[offset : offset + 10])
        offset += 10
        rdata = packet[offset : offset + rdlength]
        record = {"name": name, "type": rtype}
        if rtype == PTR:
            record["target"], _ = read_name(packet, offset)
        elif rtype == SRV:
            priority, weight, port = struct.unpack(">HHH", rdata[:6])
            record["port"] = port
            record["target"], _ = read_name(packet, offset + 6)
        elif rtype == TXT:
            record["txt"] = {}
            cursor = 0
            while cursor < len(rdata):
                item_len = rdata[cursor]
                item = rdata[cursor + 1 : cursor + 1 + item_len].decode("utf-8", "replace")
                if "=" in item:
                    key, value = item.split("=", 1)
                    record["txt"][key] = value
                cursor += 1 + item_len
        elif rtype == A:
            record["address"] = socket.inet_ntoa(rdata)
        records.append(record)
        offset += rdlength
    return records


def main() -> None:
    timeout = float(sys.argv[1]) if len(sys.argv) > 1 else 1.5
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("0.0.0.0", 0))
    sock.settimeout(timeout)

    instances: list[str] = []
    all_records: list[dict] = []
    sock.sendto(build_query([(SERVICE, PTR)]), MDNS_GROUP)
    end = time.time() + timeout
    while time.time() < end:
        try:
            packet, _ = sock.recvfrom(4096)
        except socket.timeout:
            break
        for record in parse_records(packet):
            all_records.append(record)
            if record["type"] == PTR and record["target"] not in instances:
                instances.append(record["target"])

    for instance in instances:
        sock.sendto(build_query([(instance, SRV), (instance, TXT)]), MDNS_GROUP)
        end = time.time() + timeout
        while time.time() < end:
            try:
                packet, _ = sock.recvfrom(4096)
            except socket.timeout:
                break
            all_records.extend(parse_records(packet))

    if not all_records:
        print("no mDNS answers (service not advertised?)")
        sys.exit(1)
    for record in all_records:
        if record["type"] == PTR:
            print(f"PTR {record['name']} -> {record['target']}")
        elif record["type"] == SRV:
            print(f"SRV {record['name']} -> {record['target']}:{record['port']}")
        elif record["type"] == TXT:
            print(f"TXT {record['name']} -> {record['txt']}")
        elif record["type"] == A:
            print(f"A   {record['name']} -> {record['address']}")


if __name__ == "__main__":
    main()
