import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from services import Service, ServiceError, ServiceManager, load_inventory


class SystemdFixture:
    def __init__(self, root):
        self.root = root
        self.active = False
        self.enabled = False
        self.fail_once = None
        self.operations = []
        self.listeners = []

    def __call__(self, args, *, check, text=False, capture_output=False):
        if args[0] == "ss":
            output = "".join(f"LISTEN 0 16 {endpoint} 0.0.0.0:*\n" for endpoint in self.listeners)
            return subprocess.CompletedProcess(args, 0, output)
        operation = args[1]
        self.operations.append(operation)
        if operation == self.fail_once:
            self.fail_once = None
            raise subprocess.CalledProcessError(1, args)
        if operation == "show":
            installed = (self.root / args[2]).is_file()
            fragment = self.root / args[2] if installed else ""
            enabled = ("enabled" if self.enabled else "disabled") if installed else ""
            active = "active" if self.active else "inactive"
            return subprocess.CompletedProcess(args, 0, f"FragmentPath={fragment}\nUnitFileState={enabled}\nActiveState={active}\n")
        if operation == "enable":
            self.enabled = True
            if "--now" in args:
                self.active = True
        elif operation == "disable":
            self.enabled = False
            if "--now" in args:
                self.active = False
        elif operation == "restart":
            self.active = True
        elif operation == "stop":
            self.active = False
        elif operation == "is-active" and not self.active:
            raise subprocess.CalledProcessError(3, args)
        return subprocess.CompletedProcess(args, 0)


class LifecycleTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.unit_root = root / "systemd"
        self.bundle = root / "bundle"
        self.service = Service("fixture.service", ("40-release.conf",))
        self.unit, self.drop_in = self.service.paths()
        self.systemd = SystemdFixture(self.unit_root)
        self.manager = ServiceManager(self.unit_root, self.systemd)
        (self.bundle / self.drop_in).parent.mkdir(parents=True)
        (self.bundle / self.unit).write_text("[Service]\nExecStart=/usr/bin/sleep 100\n")
        (self.bundle / self.drop_in).write_text("[Service]\nWorkingDirectory=/tmp\n")

    def test_install_update_delete_and_preserve_unmanaged_files(self):
        self.manager.change("install", self.service, self.bundle)
        self.assertTrue(self.systemd.enabled and self.systemd.active)
        self.assertEqual((self.unit_root / self.drop_in).read_bytes(), (self.bundle / self.drop_in).read_bytes())
        extra = self.unit_root / self.drop_in.parent / "80-admin.conf"
        extra.write_text("[Service]\nUMask=0077\n")
        self.manager.change("install", self.service, self.bundle)
        self.assertNotIn("restart", self.systemd.operations)
        (self.bundle / self.drop_in).write_text("[Service]\nWorkingDirectory=/var/tmp\n")
        with self.assertRaisesRegex(ServiceError, "Use update"):
            self.manager.change("install", self.service, self.bundle)
        self.manager.change("update", self.service, self.bundle)
        self.assertIn("restart", self.systemd.operations)
        self.assertEqual((self.unit_root / self.drop_in).read_bytes(), (self.bundle / self.drop_in).read_bytes())
        self.manager.change("delete", self.service)
        self.manager.change("delete", self.service)
        self.assertFalse(self.systemd.active or self.systemd.enabled)
        self.assertFalse((self.unit_root / self.unit).exists())
        self.assertFalse((self.unit_root / self.drop_in).exists())
        self.assertTrue(extra.exists())

    def test_update_failure_restores_files_and_running_state(self):
        self.manager.change("install", self.service, self.bundle)
        original = (self.unit_root / self.drop_in).read_bytes()
        (self.bundle / self.drop_in).write_text("[Service]\nWorkingDirectory=/nonexistent\n")
        self.systemd.fail_once = "restart"
        with self.assertRaisesRegex(ServiceError, "were restored"):
            self.manager.change("update", self.service, self.bundle)
        self.assertEqual((self.unit_root / self.drop_in).read_bytes(), original)
        self.assertTrue(self.systemd.enabled and self.systemd.active)

    def test_install_failure_removes_the_new_unit(self):
        self.systemd.fail_once = "is-active"
        with self.assertRaisesRegex(ServiceError, "were restored"):
            self.manager.change("install", self.service, self.bundle)
        self.assertFalse((self.unit_root / self.unit).exists())
        self.assertFalse((self.unit_root / self.drop_in).exists())
        self.assertFalse(self.systemd.enabled or self.systemd.active)

    def test_missing_drop_in_stops_before_any_effect(self):
        (self.bundle / self.drop_in).unlink()
        with self.assertRaisesRegex(ServiceError, "Bundle is missing"):
            self.manager.change("install", self.service, self.bundle)
        self.assertEqual(self.systemd.operations, ["show"])
        self.assertFalse(self.unit_root.exists())

    def test_missing_registered_port_rolls_back_update(self):
        service = Service(self.service.unit, self.service.drop_ins, ("127.0.0.1:12345",))
        self.systemd.listeners = ["127.0.0.1:12345"]
        self.manager.change("install", service, self.bundle)
        original = (self.unit_root / self.drop_in).read_bytes()
        (self.bundle / self.drop_in).write_text("[Service]\nWorkingDirectory=/var/tmp\n")
        self.systemd.listeners = []
        with patch("services.time.monotonic", side_effect=(0, 11)):
            with self.assertRaisesRegex(ServiceError, "did not open its registered listeners"):
                self.manager.change("update", service, self.bundle)
        self.assertEqual((self.unit_root / self.drop_in).read_bytes(), original)
        self.assertTrue(self.systemd.enabled and self.systemd.active)

    def test_inventory_limits_lifecycle_to_registered_mission_control_units(self):
        components = load_inventory()
        self.assertEqual(Service.from_component(components["forge-ide-web-host"]).unit, "homelab-web.service")
        for component in ("portal-proxy", "forge-ssh", "forge-llama-adapter"):
            with self.subTest(component=component), self.assertRaises(ServiceError):
                Service.from_component(components[component])


if __name__ == "__main__":
    unittest.main()
