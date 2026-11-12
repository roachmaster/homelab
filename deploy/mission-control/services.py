#!/usr/bin/env python3
"""Manage registered Mission Control systemd units from reviewed service bundles."""

import argparse
from dataclasses import dataclass
import fcntl
import os
from pathlib import Path
import re
import socket
import stat
import subprocess
import sys
import tempfile
import time

import yaml


INVENTORY = Path(__file__).resolve().parents[2] / "docs/architecture/forge-ide-system-inventory.yaml"
UNIT_ROOT = Path("/etc/systemd/system")
PROPERTIES = ("LoadState", "ActiveState", "UnitFileState", "FragmentPath")
SERVICE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.@-]*\.service")
DROP_IN_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*\.conf")


class ServiceError(Exception):
    """An inventory, bundle, or lifecycle error."""


def listeners(component):
    return [component[key] for key in ("listener", "ipv6_listener") if key in component]


def load_inventory(path=INVENTORY):
    with path.open() as source:
        inventory = yaml.safe_load(source)
    components = {}
    ports = set()
    for item in inventory["components"]:
        component = item["id"]
        if component in components:
            raise ServiceError(f"Duplicate inventory component {component}.")
        components[component] = item
        for endpoint in listeners(item):
            address, separator, port = endpoint.rpartition(":")
            if not address or not separator or not port.isdecimal() or not 1 <= int(port) <= 65535:
                raise ServiceError(f"Invalid listener for {component}: {endpoint}")
            key = item["placement"], address, int(port)
            if key in ports:
                raise ServiceError(f"Duplicate listener on {item['placement']}: {endpoint}")
            ports.add(key)
    return components


@dataclass(frozen=True)
class Service:
    unit: str
    drop_ins: tuple[str, ...]
    tcp_listeners: tuple[str, ...] = ()

    @classmethod
    def from_component(cls, item):
        if item["placement"] != "mission-control" or "lifecycle" not in item:
            raise ServiceError(f"{item['id']} has no managed Mission Control unit.")
        unit = item["service"]
        drop_ins = tuple(item["lifecycle"].get("drop_ins", ()))
        if not SERVICE_NAME.fullmatch(unit) or len(set(drop_ins)) != len(drop_ins):
            raise ServiceError(f"Invalid service definition for {item['id']}.")
        if any(not DROP_IN_NAME.fullmatch(name) for name in drop_ins):
            raise ServiceError(f"Invalid drop-in name for {item['id']}.")
        return cls(unit, drop_ins, tuple(listeners(item)))

    def paths(self):
        return (Path(self.unit), *(Path(self.unit + ".d") / name for name in self.drop_ins))


def read_file(path):
    if path.is_symlink() or path.parent.is_symlink():
        raise ServiceError(f"Expected a regular unit file at {path}.")
    try:
        metadata = path.stat()
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(metadata.st_mode):
        raise ServiceError(f"Expected a regular unit file at {path}.")
    return path.read_bytes(), stat.S_IMODE(metadata.st_mode)


def write_file(path, contents):
    if contents is None:
        path.unlink(missing_ok=True)
        return
    data, mode = contents
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as handle:
        temporary = Path(handle.name)
        try:
            handle.write(data)
            handle.flush()
            os.fchmod(handle.fileno(), mode)
            os.fsync(handle.fileno())
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def listening_endpoints(run=subprocess.run):
    output = run(["ss", "-H", "-ltn"], check=True, text=True, capture_output=True)
    return {fields[3] for row in output.stdout.splitlines() if len(fields := row.split()) >= 4}


class ServiceManager:
    def __init__(self, unit_root=UNIT_ROOT, run=subprocess.run):
        self.unit_root = unit_root
        self.run = run

    def systemctl(self, *args, capture=False):
        return self.run(["systemctl", *args], check=True, text=True, capture_output=capture)

    def status(self, unit):
        output = self.systemctl("show", unit, *("--property=" + prop for prop in PROPERTIES), capture=True)
        return dict(line.split("=", 1) for line in output.stdout.splitlines() if "=" in line)

    def verify_active(self, service):
        deadline = time.monotonic() + 10
        expected = set(service.tcp_listeners)
        while True:
            self.systemctl("is-active", "--quiet", service.unit)
            missing = expected - listening_endpoints(self.run) if expected else set()
            if not missing:
                return
            if time.monotonic() >= deadline:
                raise ServiceError(f"{service.unit} did not open its registered listeners: {', '.join(sorted(missing))}")
            time.sleep(0.25)

    def verify_bundle(self, service, contents):
        # Verify the exact bytes to be installed, including registered drop-ins.
        with tempfile.TemporaryDirectory(prefix="homelab-unit-") as directory:
            root = Path(directory)
            for relative, value in contents.items():
                write_file(root / relative, value)
            self.run(["systemd-analyze", "verify", str(root / service.unit)], check=True)

    def restore(self, service, previous, state, action, service_changed):
        errors = []

        def attempt(operation, *args):
            try:
                operation(*args)
            except (OSError, subprocess.CalledProcessError) as error:
                errors.append(str(error))

        enabled = state["UnitFileState"] in ("enabled", "enabled-runtime")
        active = state["ActiveState"] == "active"
        # A newly enabled unit must be disabled while systemd can still find it.
        if service_changed and action == "install":
            if not enabled:
                attempt(self.systemctl, "disable", "--now", service.unit)
            elif not active:
                attempt(self.systemctl, "stop", service.unit)
        for relative, contents in previous.items():
            attempt(write_file, self.unit_root / relative, contents)
        attempt(self.systemctl, "daemon-reload")
        if service_changed and action == "delete" and enabled:
            mode = ["--runtime"] if state["UnitFileState"] == "enabled-runtime" else []
            attempt(self.systemctl, "enable", *mode, service.unit)
        if service_changed and active:
            attempt(self.systemctl, "restart", service.unit)
        if errors:
            raise ServiceError(", ".join(errors))

    def change(self, action, service, bundle=None):
        unit = service.unit
        state = self.status(unit)
        destination = self.unit_root / unit
        if state["FragmentPath"] and Path(state["FragmentPath"]) != destination:
            raise ServiceError(f"{unit} is loaded from {state['FragmentPath']}.")
        previous = {relative: read_file(self.unit_root / relative) for relative in service.paths()}
        installed = previous[Path(unit)] is not None
        if action == "delete" and not any(previous.values()):
            return
        if action == "update" and not installed:
            raise ServiceError(f"{unit} is absent. Use install.")
        desired = {}
        if action != "delete":
            for relative, old in previous.items():
                candidate = read_file(bundle / relative)
                if candidate is None:
                    raise ServiceError(f"Bundle is missing {relative}.")
                data, _ = candidate
                desired[relative] = data, old[1] if old else 0o644
            if action == "install" and any(
                old is not None and old != desired[relative] for relative, old in previous.items()
            ):
                raise ServiceError(f"{unit} has different installed files. Use update.")
            if desired != previous:
                self.verify_bundle(service, desired)

        active = state["ActiveState"] == "active"
        already_enabled = state["UnitFileState"] == "enabled"
        if desired == previous and (action == "update" or action == "install" and active and already_enabled):
            if active:
                self.verify_active(service)
            return

        service_changed = False
        try:
            if action == "delete":
                if installed:
                    mode = ["--runtime"] if state["UnitFileState"] == "enabled-runtime" else []
                    service_changed = True
                    self.systemctl("disable", "--now", *mode, unit)
                for relative in previous:
                    write_file(self.unit_root / relative, None)
                self.systemctl("daemon-reload")
                return
            if desired != previous:
                for relative, contents in desired.items():
                    write_file(self.unit_root / relative, contents)
                self.systemctl("daemon-reload")
            if action == "install":
                service_changed = True
                self.systemctl("enable", "--now", unit)
            elif desired != previous and state["ActiveState"] == "active":
                service_changed = True
                self.systemctl("restart", unit)
            if action == "install" or state["ActiveState"] == "active":
                self.verify_active(service)
        except (ServiceError, OSError, subprocess.CalledProcessError) as error:
            try:
                self.restore(service, previous, state, action, service_changed)
            except ServiceError as rollback_error:
                raise ServiceError(f"{action} failed for {unit}. Rollback also failed: {rollback_error}") from error
            raise ServiceError(f"{action} failed for {unit}. The previous unit files and service state were restored. {error}") from error


def show_inventory(components):
    for item in components.values():
        for endpoint in listeners(item):
            unit = item.get("service", "")
            print(f"{item['placement']:16} {endpoint:29} {item['id']:27} {unit}")


def show_status(manager, components, selected):
    endpoints = listening_endpoints()
    for item in components.values():
        if item["placement"] != "mission-control" or "service" not in item:
            continue
        if selected and item["id"] != selected:
            continue
        state = manager.status(item["service"])
        ports = " ".join(f"{endpoint}={'listening' if endpoint in endpoints else 'closed'}" for endpoint in listeners(item))
        print(f"{item['id']:27} {state['ActiveState']:10} {state['UnitFileState']:10} {ports}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    actions.add_parser("list", help="List registered TCP listeners on all hosts")
    status = actions.add_parser("status", help="Check the actual Mission Control services and listeners")
    status.add_argument("service", nargs="?", help="Inventory component ID")
    for action in ("install", "update", "delete"):
        command = actions.add_parser(action)
        command.add_argument("service", help="Inventory component ID")
        if action != "delete":
            command.add_argument("bundle", type=Path, help="Directory containing the reviewed unit and drop-ins")
    args = parser.parse_args()

    components = load_inventory()
    manager = ServiceManager()
    if args.action == "list":
        show_inventory(components)
        return
    if args.service and args.service not in components:
        raise ServiceError(f"Unknown inventory component {args.service}.")
    if args.action == "status":
        if args.service:
            item = components[args.service]
            if item["placement"] != "mission-control" or "service" not in item:
                raise ServiceError(f"{args.service} has no Mission Control unit to check.")
        show_status(manager, components, args.service)
        return

    if socket.gethostname().split(".")[0] != "mission-control" or os.geteuid() != 0:
        raise ServiceError("Run service mutations on Mission Control with sudo.")
    service = Service.from_component(components[args.service])
    with open("/run/homelab-services.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        manager.change(args.action, service, getattr(args, "bundle", None))
    print(f"{args.action} completed for {service.unit}.")


if __name__ == "__main__":
    try:
        main()
    except (ServiceError, OSError, subprocess.CalledProcessError, yaml.YAMLError, KeyError) as error:
        sys.exit(str(error))
