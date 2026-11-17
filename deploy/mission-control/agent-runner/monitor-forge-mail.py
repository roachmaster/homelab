#!/usr/bin/env python3
"""Sample the Forge Mail model from its Linux service account."""

import argparse
from datetime import datetime, timezone
import fcntl
from http.client import HTTPConnection, HTTPException
import json
import os
from pathlib import Path
import re
import stat
import time


PROC = Path("/proc")
SYS = Path("/sys")
MODEL = "gpt-oss-120b"
UNIT = "forge-llama.service"
CLOCK_TICKS = os.sysconf("SC_CLK_TCK")
MAX_LOG_BYTES = 8 * 1024 * 1024
ENGINES = ("ccs", "bcs", "rcs", "vcs", "vecs")
GPU_REGIONS = ("vram0", "vram1")


def read(path):
    try:
        return path.read_text(errors="replace")
    except OSError:
        return ""


def fields(data, separator=":"):
    return {key: value.strip() for line in data.splitlines()
            for key, found, value in [line.partition(separator)] if found}


def number(value):
    items = value.split()
    return int(items[0]) if items and items[0].lstrip("-").isdigit() else None


def byte_count(value):
    items = value.split()
    if not items or not items[0].isdigit():
        return None
    unit = items[1] if len(items) > 1 else ""
    scale = {"": 1, "kB": 1024, "KiB": 1024, "MiB": 1024 * 1024}.get(unit)
    return int(items[0]) * scale if scale is not None else None


def collect_fields(data, names, parse):
    return {name.lower(): parse(data.get(name, "")) for name in names}


def locate_model():
    for process in PROC.glob("[0-9]*"):
        try:
            if process.stat().st_uid != os.getuid() or read(process / "comm").strip() != "llama-server":
                continue
            cgroup = next((line[3:] for line in read(process / "cgroup").splitlines()
                           if line.startswith("0::")), "")
            if not cgroup.startswith("/") or Path(cgroup).name != UNIT:
                continue
            argv = read(process / "cmdline").split("\0")
            if argv[argv.index("--alias") + 1] != MODEL:
                continue
            port = int(argv[argv.index("--port") + 1])
            model_file = Path(argv[argv.index("--model") + 1])
        except (OSError, IndexError, ValueError):
            continue
        return process, SYS / "fs/cgroup" / cgroup.lstrip("/"), port, model_file
    return None


def backend_connections(process, port):
    sockets = set()
    for fd in (process / "fd").glob("[0-9]*"):
        try:
            target = os.readlink(fd)
        except OSError:
            continue
        if target.startswith("socket:[") and target.endswith("]"):
            sockets.add(target[8:-1])
    local = f"0100007F:{port:04X}"
    return sum(row[1] == local and row[3] == "01" and row[9] in sockets
               for line in read(process / "net/tcp").splitlines()[1:]
               if len(row := line.split()) > 9)


def model_phase(port, connections):
    if connections:
        return "busy"
    health = HTTPConnection("127.0.0.1", port, timeout=0.5)
    try:
        health.request("GET", "/health")
        return "idle" if health.getresponse().status == 200 else "loading"
    except (OSError, HTTPException):
        return "loading"
    finally:
        health.close()


def process_ticks(process):
    values = read(process / "stat").rpartition(")")[2].split()
    return (int(values[19]), int(values[11]) + int(values[12])) if len(values) > 19 else (None, None)


def host_ticks():
    cpu = read(PROC / "stat").splitlines()
    values = [int(value) for value in cpu[0].split()[1:9]] if cpu else []
    if len(values) != 8:
        return None, None, None
    total = sum(values)
    return total, total - values[3] - values[4], values[4]


def gpu_clients(process, pci):
    clients = {}
    for fdinfo in (process / "fdinfo").glob("[0-9]*"):
        row = fields(read(fdinfo))
        client = row.get("drm-client-id", "")
        if row.get("drm-driver") == "xe" and row.get("drm-pdev") == pci and client.isdecimal():
            clients.setdefault(client, row)
    return clients


def disk_counters(model_file, block_device):
    if not block_device:
        try:
            device_id = model_file.stat().st_dev
            block = (SYS / "dev/block" / f"{os.major(device_id)}:{os.minor(device_id)}").resolve(strict=True)
        except OSError:
            return None, ()
    else:
        block = SYS / "class/block" / block_device
    raw = read(block / "stat").split()
    if len(raw) < 10:
        return None, ()
    return block.name, (int(raw[2]) * 512, int(raw[6]) * 512, int(raw[9]))


def snapshot(pci, block_device, include_idle=False):
    model = locate_model()
    if model is None:
        return None
    process, cgroup, port, model_file = model
    identity, cpu = process_ticks(process)
    if identity is None:
        return None
    connections = backend_connections(process, port)
    phase = model_phase(port, connections)
    if phase == "idle" and not include_idle:
        return None
    gpu = SYS / "bus/pci/devices" / pci
    temp = {}
    for label in gpu.glob("hwmon/hwmon*/temp*_label"):
        key = read(label).strip()
        if key in {"pkg", "vram"}:
            temp[key] = number(read(label.with_name(label.name.replace("_label", "_input"))))
    gt = {path.name: {"idle_ms": number(read(path / "gtidle/idle_residency_ms")),
                      "frequency_mhz": number(read(path / "freq0/act_freq"))}
          for path in (gpu / "tile0").glob("gt[0-9]") if path.name in {"gt0", "gt1"}}
    clients = gpu_clients(process, pci)
    system_cpu = host_ticks()
    cgroup_cpu = fields(read(cgroup / "cpu.stat"), " ")
    disk_name, disk = disk_counters(model_file, block_device)
    row = {
        "utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "model": MODEL,
        "pid": int(process.name),
        "phase": phase,
        "backend_connections": connections,
        "ram_bytes": collect_fields(fields(read(PROC / "meminfo")),
                                    ("MemTotal", "MemAvailable", "Cached", "SwapTotal", "SwapFree"), byte_count),
        "llama_ram_bytes": collect_fields(fields(read(process / "status")),
                                          ("VmRSS", "RssAnon", "RssFile", "VmSwap"), byte_count),
        "cgroup_ram_bytes": {name: number(read(cgroup / f"memory.{name}")) for name in ("current", "peak")},
        "gpu": {"pci": pci, "drm_clients": len(clients),
                "resident_vram_bytes": sum(byte_count(info.get(f"drm-resident-{region}", "")) or 0
                                           for info in clients.values() for region in GPU_REGIONS) if clients else None,
                "temperature_c": {key: value / 1000 for key, value in temp.items() if value is not None},
                "gt_frequency_mhz": {key: value["frequency_mhz"] for key, value in gt.items()}},
        "disk": {"device": disk_name},
    }
    # Discard reads that overlapped a process exit or PID reuse.
    if process_ticks(process)[0] != identity:
        return None
    counters = (cpu, number(cgroup_cpu.get("usage_usec", "")), system_cpu, clients, gt, disk_name, disk)
    return (int(process.name), identity), time.monotonic(), counters, row


def delta(value, previous):
    return value - previous if value is not None and previous is not None and value >= previous else None


def rate(amount, base, scale):
    return round(amount * scale / base, 2) if amount is not None and base and base > 0 else None


def add_rates(sample, previous):
    identity, observed_at, counters, row = sample
    if previous is None or identity != previous[0]:
        return
    elapsed = observed_at - previous[1]
    cpu, cgroup_cpu, host, clients, gt, disk_name, disk = counters
    old_cpu, old_cgroup, old_host, old_clients, old_gt, old_disk_name, old_disk = previous[2]
    host_total = delta(host[0], old_host[0])
    row["cpu_percent"] = {"host_busy": rate(delta(host[1], old_host[1]), host_total, 100),
                          "host_iowait": rate(delta(host[2], old_host[2]), host_total, 100),
                          "llama_one_core": rate(delta(cpu, old_cpu), elapsed, 100 / CLOCK_TICKS),
                          "cgroup_one_core": rate(delta(cgroup_cpu, old_cgroup), elapsed, 100 / 1_000_000)}
    row["gpu"]["gt_busy_percent"] = {
        name: round(100 - idle, 2) for name, values in gt.items() if name in old_gt
        if (idle := rate(delta(values["idle_ms"], old_gt[name]["idle_ms"]), elapsed, 0.1)) is not None
        if 0 <= idle <= 100}
    engine_rates = {}
    for client in clients.keys() & old_clients.keys():
        current, old = clients[client], old_clients[client]
        for engine in ENGINES:
            busy_key, total_key = f"drm-cycles-{engine}", f"drm-total-cycles-{engine}"
            busy = delta(number(current.get(busy_key, "")), number(old.get(busy_key, "")))
            total = delta(number(current.get(total_key, "")), number(old.get(total_key, "")))
            capacity = number(current.get(f"drm-engine-capacity-{engine}", "")) or 1
            utilization = rate(busy, total, 100 / capacity)
            if utilization is not None:
                engine_rates[engine] = min(100.0, round(engine_rates.get(engine, 0) + utilization, 2))
    row["gpu"]["llama_engine_busy_percent"] = engine_rates
    if disk and old_disk and disk_name == old_disk_name:
        row["disk"].update(read_bytes_sec=rate(delta(disk[0], old_disk[0]), elapsed, 1),
                           write_bytes_sec=rate(delta(disk[1], old_disk[1]), elapsed, 1),
                           busy_percent=rate(delta(disk[2], old_disk[2]), elapsed, 0.1))


def append_row(directory, row):
    output = directory / "telemetry.jsonl"
    line = (json.dumps(row, separators=(",", ":"), sort_keys=True) + "\n").encode()
    if output.exists() and output.stat().st_size + len(line) > MAX_LOG_BYTES:
        first, second = directory / "telemetry.1.jsonl", directory / "telemetry.2.jsonl"
        if first.exists():
            first.replace(second)
        output.replace(first)
    with os.fdopen(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW, 0o600), "ab") as log:
        log.write(line)


def sample_window(args):
    start = time.monotonic()
    previous = None
    for offset in range(0, args.duration, args.interval):
        time.sleep(max(0.0, start + offset - time.monotonic()))
        sample = snapshot(args.pci_device, args.block_device, args.include_idle)
        if sample is not None:
            add_rates(sample, previous)
            append_row(args.output_dir, sample[3])
        previous = sample


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--duration", type=int, default=55, help="sample window in seconds (default: 55)")
    parser.add_argument("--interval", type=int, default=5, help="sample interval in seconds (default: 5)")
    parser.add_argument("--pci-device", default="0000:03:00.0", help="Intel Arc PCI address")
    parser.add_argument("--block-device", help="disk override such as nvme0n1; defaults to the model filesystem")
    parser.add_argument("--include-idle", action="store_true", help="also record an idle resident Mail model")
    parser.add_argument("--output-dir", type=Path, default=Path.home() / ".local/state/forge-mail-monitor")
    args = parser.parse_args()
    if not 1 <= args.interval <= args.duration <= 60:
        parser.error("require 1 <= interval <= duration <= 60 seconds")
    if not re.fullmatch(r"[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]", args.pci_device):
        parser.error("invalid PCI device address")
    if args.block_device and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,31}", args.block_device):
        parser.error("invalid block device name")
    if not args.output_dir.is_absolute():
        parser.error("output directory must be absolute")
    os.umask(0o077)
    args.output_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    info = args.output_dir.lstat()
    if info.st_uid != os.getuid() or not stat.S_ISDIR(info.st_mode) or info.st_mode & 0o077:
        parser.error("output directory must be private and owned by the service user")
    lock_path = args.output_dir / "sampling.lock"
    with os.fdopen(os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600), "wb") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        sample_window(args)


if __name__ == "__main__":
    main()
