"""Strict usage ingress/store boundary; no process or provider imports required."""

from copy import deepcopy
from email.message import Message
import sys
import unittest
from unittest.mock import Mock

from queue_store import Rejected
import usage_api as api
from test_usage_provider import provider_row
from work_config import parse_work_config


def config():
    return parse_work_config({
        "owner": "fixture", "allowed_projects": ["homelab", "docs"], "allowed_modes": ["plan"],
        "profiles": {"codex-primary": {"backend": "codex", "model": "fixture-model", "resource_profile": "fixture"}},
    })


class UsageApiTest(unittest.TestCase):
    def test_factory_is_effect_free_optional_and_injects_exact_store(self):
        selected = Mock()
        self.assertIsNone(api.UsageServiceFactory(None, selected).create())
        selected.assert_not_called()
        cfg = config()
        selected.return_value.overview.return_value = {"project_alias": "homelab", "providers": [provider_row()]}
        factory = api.UsageServiceFactory(cfg, selected)
        selected.assert_not_called()
        service = factory.create()
        selected.assert_called_once_with(cfg)
        result = service.overview(api.UsageOverview("homelab"))
        self.assertEqual(result["providers"][0]["alias"], "codex-primary")
        selected.return_value.overview.assert_called_once_with(project_alias="homelab", profiles=("codex-primary",))

    def test_all_request_contracts_and_unknown_fields_are_strict(self):
        common = {"request_id": "request-1", "project_alias": "homelab"}
        self.assertEqual(api.decode(api.Destination.OVERVIEW, "request-1", {**common, "kind": "usage_overview"}),
                         api.UsageOverview("homelab"))
        self.assertEqual(api.decode(api.Destination.DETAIL, "request-1", {**common, "kind": "usage_detail", "agent_profile": "codex-primary"}),
                         api.UsageDetail("homelab", "codex-primary"))
        for optional in ({}, {"agent_profile": None}):
            self.assertEqual(api.decode(api.Destination.REFRESH, "request-1", {**common, "kind": "usage_refresh", **optional}),
                             api.UsageRefresh("homelab", None))
        bodies = [None, [], {**common, "kind": "usage_overview", "owner": "other"},
                  {**common, "kind": "usage_overview", "request_id": "other"},
                  {**common, "kind": "usage_detail"},
                  {**common, "kind": "usage_refresh", "agent_profile": []}]
        for body in bodies:
            with self.assertRaises(Rejected) as error:
                api.decode(api.Destination.OVERVIEW, "request-1", body)
            self.assertEqual(error.exception.status, 400)

    def test_header_versions_duplicates_and_unsupported_destinations(self):
        headers = Message()
        for name, value in {"Forge-Destination": "ide.usage.overview", "Forge-Request-Id": "r1",
                            "Forge-Envelope-Version": "1", "Forge-Contract-Version": "1"}.items():
            headers[name] = value
        self.assertTrue(api.handles(headers))
        self.assertIs(api.destination(headers), api.Destination.OVERVIEW)
        headers["Forge-Destination"] = "ide.usage.detail"
        with self.assertRaises(Rejected) as error:
            api.destination(headers)
        self.assertEqual(error.exception.status, 400)
        del headers["Forge-Destination"]
        headers["Forge-Destination"] = "ide.usage.generate"
        with self.assertRaises(Rejected) as error:
            api.destination(headers)
        self.assertEqual(error.exception.status, 404)

    def test_denied_project_or_profile_never_reaches_store(self):
        store = Mock()
        service = api.UsageService(config(), store)
        for command in (api.UsageOverview("foreign"), api.UsageDetail("homelab", "foreign"),
                        api.UsageDetail("foreign", "codex-primary")):
            with self.assertRaises(Rejected) as error:
                service.overview(command)
            self.assertEqual(error.exception.status, 403)
        for command in (api.UsageRefresh("foreign", None), api.UsageRefresh("homelab", "foreign")):
            with self.assertRaises(Rejected) as error:
                service.refresh(command)
            self.assertEqual(error.exception.status, 403)
        self.assertEqual(store.mock_calls, [])

    def test_malformed_wrong_scope_and_private_store_rows_fail_closed(self):
        store = Mock()
        service = api.UsageService(config(), store)
        mutations = [lambda row: row.update(alias="foreign"), lambda row: row.update(model="other-model"),
                     lambda row: row.update(provider="antigravity"), lambda row: row.update(email="PRIVATE"),
                     lambda row: row["jobs"].update(total_tokens=99999999999999999999)]
        for mutate in mutations:
            row = provider_row()
            mutate(row)
            store.overview.return_value = {"project_alias": "homelab", "providers": [row]}
            with self.assertRaises(Rejected) as error:
                service.overview(api.UsageOverview("homelab"))
            self.assertEqual((error.exception.status, error.exception.code), (503, "unavailable"))
        store.overview.return_value = {"project_alias": "foreign", "providers": [provider_row()]}
        with self.assertRaises(Rejected):
            service.overview(api.UsageOverview("homelab"))

    def test_dispatch_correlation_detail_and_refresh_are_metadata_only(self):
        store = Mock()
        store.overview.return_value = {"project_alias": "homelab", "providers": [provider_row()]}
        store.request_refresh.return_value = {"project_alias": "homelab", "refresh_id": "18d5f913-cab9-453f-bbf6-84cccf8df9ea",
                                               "profiles": ["codex-primary"], "coalesced": True}
        service = api.UsageService(config(), store)
        status, result = api.dispatch(api.Destination.DETAIL, "r-new", api.UsageDetail("homelab", "codex-primary"), service)
        self.assertEqual((status, result["event"]["kind"], result["event"]["request_id"]), (200, "usage_detail", "r-new"))
        store.overview.assert_called_once_with(project_alias="homelab", profiles=("codex-primary",))
        status, result = api.dispatch(api.Destination.REFRESH, "r-refresh", api.UsageRefresh("homelab", None), service)
        self.assertEqual((status, result["event"]["kind"]), (202, "usage_refresh_queued"))
        self.assertTrue(result["event"]["coalesced"])
        self.assertEqual(store.method_calls[-1].args, ())
        store.request_refresh.assert_called_once_with(project_alias="homelab", agent_profile=None)
        self.assertEqual({call[0] for call in store.method_calls}, {"overview", "request_refresh"})

    def test_failure_codes_sanitize_unavailable_and_internal_details(self):
        for status, code in ((400, "invalid_request"), (403, "denied"), (404, "not_found"),
                             (413, "invalid_request"), (408, "unavailable"), (500, "unavailable")):
            result_status, body = api.failed("r1", Rejected(status, "PRIVATE_CONNECTION_DETAILS"))
            self.assertEqual(result_status, status if status in (400, 403, 404) else 400 if status == 413 else 503)
            self.assertEqual(body["event"]["code"], code)
            self.assertNotIn("PRIVATE", str(body))
        with self.assertRaises(Rejected) as error:
            api.dispatch(api.Destination.OVERVIEW, "r1", api.UsageOverview("homelab"), None)
        self.assertEqual(error.exception.status, 503)


if __name__ == "__main__":
    unittest.main()
