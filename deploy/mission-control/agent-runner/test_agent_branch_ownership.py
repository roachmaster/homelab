"""Tests for persistent per-agent branches, exclusive ownership, trunk rebase, CI/CD artifacts & recovery."""

from contextlib import closing
from pathlib import Path
import subprocess
import tempfile
import unittest
from uuid import UUID, uuid4

import psycopg2

import work_agent_branch
import work_api
import work_cicd_artifacts
import work_config
import work_pg_fixture
from work_pg_fixture import PG16_AVAILABLE
import work_pipeline_projection
import work_policy
import work_publication_clients
import work_publication_config
import work_store
import work_worker_config
import work_worker_store


def _git(repo, *args, check=True):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=check,
        text=True,
        capture_output=True,
    )


def _init_bare_and_seed(tmp_root):
    remote = tmp_root / "remote.git"
    seed = tmp_root / "seed"
    subprocess.run(["git", "init", "--bare", "--initial-branch=main", str(remote)], check=True, capture_output=True)
    subprocess.run(["git", "clone", str(remote), str(seed)], check=True, capture_output=True)
    _git(seed, "config", "user.name", "Forge Test")
    _git(seed, "config", "user.email", "forge-test@example.invalid")
    (seed / "README.md").write_text("initial\n", encoding="utf-8")
    (seed / "shared.txt").write_text("base-line\n", encoding="utf-8")
    _git(seed, "add", ".")
    _git(seed, "commit", "-m", "initial commit")
    _git(seed, "push", "-u", "origin", "main")
    m0 = _git(seed, "rev-parse", "HEAD").stdout.strip()
    return remote, seed, m0


class FakePullRequestClient:
    def __init__(self):
        self.open_prs = []
        self.created = []
        self.next_number = 101

    def list_open(self, job):
        branch = job.get("branch_name")
        return [dict(pr) for pr in self.open_prs if branch is None or pr["head"]["ref"] == branch]

    def create(self, branch, head_sha):
        num = self.next_number
        self.next_number += 1
        url = f"https://github.com/roachmaster/homelab/pull/{num}"
        pr = {
            "number": num,
            "html_url": url,
            "head": {"ref": branch, "sha": head_sha},
            "base": {"ref": "main"},
        }
        self.open_prs.append(pr)
        self.created.append(pr)
        return url

    def close(self, url):
        self.open_prs = [pr for pr in self.open_prs if pr["html_url"] != url]


def _sample_work_config_dict(profile_overrides=None):
    profiles = {
        "agy-primary": {"label": "AGY", "backend": "antigravity", "model": "gemini-3", "resource_profile": work_policy.POLICY_ID},
        "codex-primary": {"label": "Codex", "backend": "codex", "model": "gpt-5", "resource_profile": work_policy.POLICY_ID},
        "grok-primary": {"label": "Grok", "backend": "grok", "model": "grok-4", "resource_profile": work_policy.POLICY_ID},
        "muse-primary": {"label": "Muse", "backend": "muse", "model": "muse-1", "resource_profile": work_policy.POLICY_ID},
        "muse-db-primary": {"label": "Muse DB", "backend": "muse", "model": "external-runner", "resource_profile": "forge-external-db-v2"},
    }
    if profile_overrides:
        for k, v in profile_overrides.items():
            profiles[k] = dict(profiles[k], **v)
    return {
        "owner": "owner@example.invalid",
        "allowed_projects": ["homelab"],
        "allowed_modes": ["plan", "implement", "review"],
        "profiles": profiles,
    }


def _sample_worker_config_dict(tmp_root, profile_overrides=None):
    profiles = {
        "agy-primary": {"backend": "antigravity", "model": "gemini-3", "modes": ["plan", "implement"]},
        "codex-primary": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "implement"]},
    }
    if profile_overrides:
        for k, v in profile_overrides.items():
            profiles[k] = dict(profiles[k], **v)
    return {
        "worker_id": "worker-1",
        "owner": "owner@example.invalid",
        "resource_profile": work_policy.POLICY_ID,
        "account": {"user": "forge", "uid": 1000, "gid": 1000, "home": str(tmp_root / "home")},
        "workspace_root": str(tmp_root / "workspaces"),
        "state_root": str(tmp_root / "state"),
        "projects": {"homelab": str(tmp_root / "repo")},
        "profiles": profiles,
        "binaries": {"antigravity": "/usr/bin/true", "codex": "/usr/bin/true"},
        "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
        "allow_implement": True,
    }


class AgentBranchConfigAndArtifactsTest(unittest.TestCase):
    def test_default_persistent_branches_and_uniqueness_validation(self):
        cfg = work_config.parse_work_config(_sample_work_config_dict())
        self.assertEqual(cfg.profiles["agy-primary"].branch, "forge/agent/agy-primary")
        self.assertEqual(cfg.profiles["codex-primary"].branch, "forge/agent/codex-primary")
        self.assertEqual(cfg.profiles["grok-primary"].branch, "forge/agent/grok-primary")
        self.assertEqual(cfg.profiles["muse-primary"].branch, "forge/agent/muse-primary")
        self.assertEqual(cfg.profiles["muse-db-primary"].branch, "forge/agent/muse-db-primary")

        cfg_custom = work_config.parse_work_config(
            _sample_work_config_dict({
                "agy-primary": {"branch": "forge/agent/custom-agy"},
                "muse-db-primary": {"branch": "feature/muse"},
            })
        )
        self.assertEqual(cfg_custom.profiles["agy-primary"].branch, "forge/agent/custom-agy")
        self.assertEqual(cfg_custom.profiles["muse-db-primary"].branch, "feature/muse")

        # Duplicate branch across two profiles fails fast
        with self.assertRaises(ValueError):
            work_config.parse_work_config(
                _sample_work_config_dict({"codex-primary": {"branch": "forge/agent/agy-primary"}})
            )

        # Protected branch 'main' fails fast
        with self.assertRaises(ValueError):
            work_config.parse_work_config(
                _sample_work_config_dict({"agy-primary": {"branch": "main"}})
            )

        # Invalid refs and ephemeral prefix rejected
        for bad_ref in (
            "forge/agent/bad..ref",
            "forge/agent/bad space",
            "-leading-dash",
            "forge/agent/end.lock",
            "forge/task-1234",
            "feature/native-disallowed",
        ):
            with self.assertRaises(ValueError):
                work_agent_branch.validate_persistent_branch(bad_ref)

    def test_worker_advertised_profiles_keeps_sql_schema_keys(self):
        with tempfile.TemporaryDirectory(prefix="forge-wcfg-") as tmp:
            wcfg = work_worker_config.parse_config(_sample_worker_config_dict(Path(tmp)))
            self.assertEqual(wcfg.profiles["agy-primary"].branch, "forge/agent/agy-primary")
            self.assertEqual(wcfg.profiles["codex-primary"].branch, "forge/agent/codex-primary")
            advertised = wcfg.advertised_profiles()
            for entry in advertised:
                self.assertEqual(set(entry.keys()), {"alias", "backend", "model", "modes"})

    def test_publication_branch_policy_defaults_and_ephemeral_fallback(self):
        with tempfile.TemporaryDirectory(prefix="forge-pcfg-") as tmp:
            wcfg = work_worker_config.parse_config(_sample_worker_config_dict(Path(tmp)))
            policy = work_agent_branch.AgentBranchPolicy.from_worker_and_overrides(wcfg.profiles)
            self.assertEqual(policy.policy, work_agent_branch.PERSISTENT_BRANCH_POLICY)
            self.assertEqual(policy.resolve_branch("homelab", "agy-primary"), "forge/agent/agy-primary")

            pub_cfg = type("Cfg", (), {
                "branch_policy": work_agent_branch.PERSISTENT_BRANCH_POLICY,
                "resolved_branch_policy": lambda self: policy,
            })()
            job = {
                "project_alias": "homelab",
                "task_id": "11111111-1111-4111-8111-111111111111",
                "publication_id": "22222222-2222-4222-8222-222222222222",
                "agent_profile": "agy-primary",
            }
            self.assertEqual(work_publication_clients.branch_name(job, pub_cfg), "forge/agent/agy-primary")

            ephemeral_cfg = type("Cfg", (), {"branch_policy": work_agent_branch.EPHEMERAL_BRANCH_POLICY})()
            self.assertEqual(
                work_publication_clients.branch_name(job, ephemeral_cfg),
                "forge/task-22222222-2222-4222-8222-222222222222",
            )

    def test_cicd_artifacts_test_parsing_redaction_and_logical_grouping(self):
        # Cargo test parsing + error excerpt + secret redaction
        raw_log = (
            b"FORGE_FIXED_CHECK_START 0\n"
            b"cargo fmt --check\n"
            b"FORGE_FIXED_CHECK_END 0 rc=0 elapsed_ms=120\n"
            b"FORGE_FIXED_CHECK_START 1\n"
            b"running 4 tests\n"
            b"test tests::ok_case ... ok\n"
            b"test tests::bad_case ... FAILED\n"
            b"error[E0308]: mismatched types\n"
            b" --> src/lib.rs:42:5\n"
            b"Bearer supersecretTOKENvalue1234567890\n"
            b"test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured\n"
            b"FORGE_FIXED_CHECK_END 1 rc=101 elapsed_ms=4321\n"
        )
        receipt = {
            "verification_profile": "forge-ci-event-v3",
            "checks": [
                {"index": 0, "returncode": 0},
                {"index": 1, "returncode": 101},
            ],
            "stages": [
                {"name": "check_0", "label": "formatting", "elapsed_ms": 120},
                {"name": "check_1", "label": "unit tests", "elapsed_ms": 4321},
            ],
            "phase_results": [
                {"index": 0, "phase_id": "formatting", "kind": "formatting"},
                {"index": 1, "phase_id": "all-features-unit-tests", "kind": "tests"},
            ],
        }
        extracted = work_cicd_artifacts.extract_stage_evidence_from_receipt(
            receipt, raw_log,
            project_alias="homelab",
            action_id=str(uuid4()),
            correlation_id=str(uuid4()),
            job_name="forge-native-verify",
            commit_sha="a" * 40,
            branch_scope="branch/forge-agent-agy-primary-0123456789abcdef",
            pipeline_scope="branch",
            receipt_sha256="b" * 64,
            build_number=42,
            branch_name="forge/agent/agy-primary",
        )
        self.assertEqual(len(extracted), 2)
        fmt_ev, _ = extracted[0]
        self.assertIsNone(fmt_ev["tests_passed"])
        self.assertEqual(fmt_ev["status"], "passed")
        self.assertIsNone(fmt_ev["error_excerpt"])

        test_ev, test_chunks = extracted[1]
        self.assertEqual(test_ev["status"], "failed")
        self.assertEqual(test_ev["tests_passed"], 3)
        self.assertEqual(test_ev["tests_failed"], 1)
        self.assertIn("error[E0308]", test_ev["error_excerpt"])
        self.assertNotIn("supersecretTOKENvalue", test_ev["error_excerpt"])
        self.assertIn("[REDACTED_TOKEN]", test_ev["error_excerpt"])
        self.assertGreaterEqual(len(test_chunks), 1)

        # ui-qa 28-group summary
        ui_passed, ui_failed, ui_skipped, ui_summary = work_cicd_artifacts.parse_test_summary(
            "", phase_id="browser-journeys", phase_kind="browser",
            ui_qa_stages=[{"name": f"group-{i}", "status": "passed"} for i in range(28)],
        )
        self.assertEqual((ui_passed, ui_failed, ui_skipped), (28, 0, 0))
        self.assertEqual(ui_summary["framework"], "ui-qa")

        # Correlation projection produces <=16 concise logical stage dots + sub_phases
        proj = work_pipeline_projection.project_correlation_cicd(
            correlation_id=test_ev["correlation_id"],
            stage_results=[fmt_ev, test_ev],
        )
        self.assertEqual(len(proj["runs"]), 1)
        self.assertLessEqual(len(proj["runs"][0]["stages"]), 16)
        unit_stage = next(s for s in proj["runs"][0]["stages"] if s["stage_id"] == "unit-tests")
        self.assertEqual(unit_stage["status"], "failed")
        self.assertEqual(unit_stage["tests_passed"], 3)
        self.assertEqual(unit_stage["tests_failed"], 1)
        self.assertTrue(unit_stage["has_console_log"])

    def test_work_api_decodes_and_rejects_cicd_stage_log_bounds(self):
        tid, cid, aid = str(uuid4()), str(uuid4()), str(uuid4())
        cmd = work_api.decode(
            work_api.Destination.CICD_STAGE_LOG, "req-1",
            {
                "kind": "work_cicd_stage_log",
                "request_id": "req-1",
                "project_alias": "homelab",
                "task_id": tid,
                "correlation_id": cid,
                "action_id": aid,
                "stage_id": "unit-tests",
                "view": "errors",
                "limit_bytes": 4096,
            },
        )
        self.assertIsInstance(cmd, work_api.WorkCicdStageLog)
        self.assertEqual(cmd.stage_id, "unit-tests")
        self.assertEqual(cmd.view, "errors")

        with self.assertRaises(work_api.Rejected):
            work_api.decode(
                work_api.Destination.CICD_STAGE_LOG, "req-2",
                {
                    "kind": "work_cicd_stage_log",
                    "request_id": "req-2",
                    "project_alias": "homelab",
                    "task_id": tid,
                    "correlation_id": cid,
                    "action_id": aid,
                    "stage_id": "unit-tests",
                    "view": "console",
                    "limit_bytes": 65536,  # > 32 KiB
                },
            )

    def test_cache_restore_status_normalizes_hit_main_seed_and_hit_branch(self):
        import work_native_jenkins

        recorded = []

        class FakeBranchProvider:
            persistent_enabled = True

            def record_sync(self, project_alias, agent_profile, **kwargs):
                recorded.append((project_alias, agent_profile, kwargs))

        delivery = type("Delivery", (), {"config": type("Cfg", (), {"results_root": None, "jenkins_job": "forge-native-verify"})()})()
        verifier = work_native_jenkins.NativeJenkinsVerification(
            config=object(), store=object(), git=object(), candidates=object(),
            delivery=delivery, supervisor=object(), branch_provider=FakeBranchProvider(),
        )
        aid, cid, tid = str(uuid4()), str(uuid4()), str(uuid4())
        binding = {
            "request": {"action_id": aid, "candidate_commit": "a" * 40, "source_branch": "forge/agent/agy-primary"},
            "scope": {"project_alias": "homelab", "task_id": tid, "correlation_id": cid},
            "source_publication": {"agent_profile": "agy-primary", "branch": "forge/agent/agy-primary"},
        }
        verifier._persist_stage_artifacts(binding, {"receipt": {"cache": {"restore_status": "hit_main_seed"}}})
        verifier._persist_stage_artifacts(binding, {"receipt": {"cache": {"restore_status": "hit_branch"}}})
        self.assertEqual([r[2]["cache_status"] for r in recorded], ["hit_main", "hit_branch"])


@unittest.skipUnless(PG16_AVAILABLE, "Disposable PostgreSQL 16 required")
class AgentBranchOwnershipPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = work_pg_fixture.DisposablePostgresCluster()
        cls.cluster.start()

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def _worker_db(self, owner="owner@example.invalid"):
        return work_worker_store.WorkerDB(
            lambda: self.cluster.connect("forge_coding_worker"),
            owner,
            reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"),
        )

    def _insert_task(self, owner, project, agent_profile, *, status="running", root_task_id=None, correlation_id=None):
        from psycopg2.extras import Json
        task_id = uuid4()
        corr_id = correlation_id or uuid4()
        cfg = work_config.parse_work_config({**_sample_work_config_dict(), "owner": owner})
        recipe = cfg.catalog().resolve(agent_profile, "implement")
        with closing(self.cluster.connect("forge_agent")) as conn:
            with conn:
                with conn.cursor() as cur:
                    cur.execute("SELECT set_config('forge.coding_owner', %s, true)", (owner,))
                    cur.execute("""
                        INSERT INTO forge_agent.coding_tasks(
                            id, correlation_id, owner_id, project_alias, idempotency_key, intent_digest,
                            base_revision, mode, agent_profile, prompt, acceptance_criteria,
                            pinned_recipe, recipe_digest, status, revision, cancelled_at, repair_root_task_id
                        ) VALUES (%s, %s, %s, %s, %s, %s, %s, 'implement', %s, 'prompt', 'criteria', %s, %s, %s, %s,
                                  CASE WHEN %s = 'cancelled' THEN clock_timestamp() ELSE NULL END, %s)
                    """, (
                        task_id, corr_id, owner, project, f"idem-{task_id}", "c" * 64, "a" * 40,
                        agent_profile, Json(recipe.to_dict()), recipe.digest, status,
                        1 if status == "queued" else 2, status, root_task_id,
                    ))
        return task_id, corr_id

    def test_overlapping_workflows_exclusive_ownership_reentrancy_and_release(self):
        owner = f"owner-{uuid4().hex[:8]}@example.invalid"
        db = self._worker_db(owner)
        policy = work_agent_branch.AgentBranchPolicy()
        provider = work_agent_branch.AgentBranchProvider(policy, db)

        task_a, corr_a = self._insert_task(owner, "homelab", "agy-primary")
        task_a2, _ = self._insert_task(owner, "homelab", "agy-primary", root_task_id=task_a)
        task_b, corr_b = self._insert_task(owner, "homelab", "agy-primary")
        task_c, corr_c = self._insert_task(owner, "homelab", "codex-primary")

        wf_a, wf_b, wf_c = str(uuid4()), str(uuid4()), str(uuid4())

        # 1. Task A acquires agy-primary -> epoch 1.
        res_a = provider.acquire({
            "project_alias": "homelab", "agent_profile": "agy-primary",
            "workflow_id": wf_a, "root_task_id": str(task_a), "task_id": str(task_a),
        })
        self.assertEqual(res_a.branch_name, "forge/agent/agy-primary")
        self.assertEqual(res_a.reservation_epoch, 1)

        # 2. Task C acquires codex-primary independently (no head-of-line blocking).
        res_c = provider.acquire({
            "project_alias": "homelab", "agent_profile": "codex-primary",
            "workflow_id": wf_c, "root_task_id": str(task_c), "task_id": str(task_c),
        })
        self.assertEqual(res_c.branch_name, "forge/agent/codex-primary")
        self.assertEqual(res_c.reservation_epoch, 1)

        # 3. Unrelated Task B on agy-primary fails closed with BranchOwnershipConflictError.
        with self.assertRaises(work_agent_branch.BranchOwnershipConflictError):
            provider.acquire({
                "project_alias": "homelab", "agent_profile": "agy-primary",
                "workflow_id": wf_b, "root_task_id": str(task_b), "task_id": str(task_b),
            })

        # 4. Same-workflow repair child Task A2 re-enters with same scope_id and preserves epoch 1.
        res_a2 = provider.acquire({
            "project_alias": "homelab", "agent_profile": "agy-primary",
            "workflow_id": wf_a, "repair_root_task_id": str(task_a), "task_id": str(task_a2),
        })
        self.assertTrue(res_a2.reentrant)
        self.assertEqual(res_a2.reservation_epoch, 1)
        self.assertEqual(res_a2.task_id, str(task_a2))

        # 5. Record publication PR #101 under epoch 1; stale epoch 99 fails verify_owner.
        pub_id = str(uuid4())
        provider.record_publication(
            res_a2,
            candidate_commit="1" * 40,
            pr_url="https://github.com/roachmaster/homelab/pull/101",
            publication_id=pub_id,
        )
        self.assertTrue(provider.verify_owner(res_a2)["valid"])
        stale_res = work_agent_branch.BranchReservation(
            project_alias="homelab", agent_profile="agy-primary",
            branch_name="forge/agent/agy-primary", branch_scope=res_a2.branch_scope,
            scope_kind="native_workflow", scope_id=wf_a,
            root_task_id=str(task_a), task_id=str(task_a2),
            reservation_epoch=99,
        )
        with self.assertRaises(work_agent_branch.BranchOwnershipConflictError):
            provider.verify_owner(stale_res)

        # 6. Merge of A releases branch (bumping epoch); Task B then acquires epoch 3.
        released = provider.release(
            res_a2, release_reason="merged",
            remote_open_pr_confirmed_none=True, merge_commit="9" * 40,
        )
        self.assertEqual(released["reservation_status"], "idle")
        self.assertEqual(released["release_reason"], "merged")
        self.assertEqual(released["fetched_main_sha"], "9" * 40)

        res_b = provider.acquire({
            "project_alias": "homelab", "agent_profile": "agy-primary",
            "workflow_id": wf_b, "root_task_id": str(task_b), "task_id": str(task_b),
        })
        self.assertGreater(res_b.reservation_epoch, res_a2.reservation_epoch)

    def test_cancelled_workflow_blocker_and_auto_release_when_pr_closes(self):
        owner = f"owner-{uuid4().hex[:8]}@example.invalid"
        db = self._worker_db(owner)
        provider = work_agent_branch.AgentBranchProvider(work_agent_branch.AgentBranchPolicy(), db)
        pr_client = FakePullRequestClient()

        task_a, _ = self._insert_task(owner, "homelab", "agy-primary", status="cancelled")
        res_a = provider.acquire({
            "project_alias": "homelab", "agent_profile": "agy-primary",
            "task_id": str(task_a),
        })
        pr_url = pr_client.create("forge/agent/agy-primary", "2" * 40)
        provider.record_publication(res_a, candidate_commit="2" * 40, pr_url=pr_url)

        # While PR is still open, reconcile_reservations must NOT release; it sets open_pr_awaiting_closure.
        summary_open = provider.reconcile_reservations(pr_client, {"homelab": "roachmaster/homelab"})
        self.assertEqual(summary_open["blocked"], 1)
        self.assertEqual(summary_open["released"], 0)
        state = db.agent_branch_get("homelab", "agy-primary")
        self.assertEqual(state["reservation_status"], "active")
        self.assertEqual(state["reservation_blocker"], "open_pr_awaiting_closure")

        # Once PR is closed on GitHub, reconcile_reservations releases automatically.
        pr_client.close(pr_url)
        summary_closed = provider.reconcile_reservations(pr_client, {"homelab": "roachmaster/homelab"})
        self.assertEqual(summary_closed["released"], 1)
        state_after = db.agent_branch_get("homelab", "agy-primary")
        self.assertEqual(state_after["reservation_status"], "idle")
        self.assertIsNone(state_after["reservation_blocker"])

    def test_git_trunk_rebase_cas_push_crash_recovery_and_conflict_handling(self):
        owner = f"owner-{uuid4().hex[:8]}@example.invalid"
        db = self._worker_db(owner)
        policy = work_agent_branch.AgentBranchPolicy()
        provider = work_agent_branch.AgentBranchProvider(policy, db)
        pr_client = FakePullRequestClient()

        with tempfile.TemporaryDirectory(prefix="forge-branch-git-") as tmp:
            tmp_root = Path(tmp)
            remote, seed, m0 = _init_bare_and_seed(tmp_root)
            wcfg = work_worker_config.parse_config({
                **_sample_worker_config_dict(tmp_root),
                "owner": owner,
                "projects": {"homelab": str(seed)},
            })
            pub_cfg = type("PubCfg", (), {
                "worker": wcfg,
                "state_root": tmp_root / "state" / "publication",
                "github_config_dir": tmp_root / "gh",
                "repositories": {"homelab": "roachmaster/homelab"},
                "commit_name": "Forge Test",
                "commit_email": "forge-test@example.invalid",
                "branch_policy": work_agent_branch.PERSISTENT_BRANCH_POLICY,
                "resolved_branch_policy": lambda self: policy,
            })()
            pub_cfg.state_root.mkdir(parents=True, exist_ok=True)
            git_provider = work_publication_clients.GitRepositoryProvider(
                pub_cfg,
                remote_url_factory=lambda _alias: str(remote),
                branch_provider=provider,
            )

            def _snapshot(files_map):
                entries = tuple(
                    work_publication_clients.SnapshotFile(path, "file", data.encode("utf-8"), False)
                    for path, data in sorted(files_map.items())
                )
                digest = "c" * 64
                return work_publication_clients.Snapshot(entries, digest)

            # Task A prepares on top of m0 and CAS-pushes to forge/agent/agy-primary.
            task_a, corr_a = self._insert_task(owner, "homelab", "agy-primary")
            pub_a = str(uuid4())
            job_a = {
                "project_alias": "homelab", "agent_profile": "agy-primary",
                "task_id": str(task_a), "correlation_id": str(corr_a),
                "publication_id": pub_a, "attempt_id": str(uuid4()),
                "base_revision": m0, "workspace_digest": "c" * 64,
            }
            res_a = provider.acquire(job_a)
            snap_a = _snapshot({"README.md": "initial\n", "shared.txt": "base-line\n", "feature_a.txt": "from A\n"})
            repo_a, head_a = git_provider.prepare_source(job_a, snap_a)
            git_provider.push(job_a, repo_a, head_a, expected_remote_head=None, reservation=res_a, pull_requests=pr_client)
            pr_101 = pr_client.create("forge/agent/agy-primary", head_a)
            provider.record_publication(res_a, candidate_commit=head_a, pr_url=pr_101, publication_id=pub_a)

            # Advance main to M1, close PR #101, release reservation.
            (seed / "main_advance.txt").write_text("merged M1\n", encoding="utf-8")
            _git(seed, "add", "main_advance.txt")
            _git(seed, "commit", "-m", "Main M1")
            _git(seed, "push", "origin", "main")
            m1 = _git(seed, "rev-parse", "HEAD").stdout.strip()
            pr_client.close(pr_101)
            provider.release(res_a, release_reason="merged", remote_open_pr_confirmed_none=True, merge_commit=m1)

            # Simulate crashed rebase-merge inside cached per-agent clone.
            cache_clone = git_provider._agent_cache_dir("homelab", "agy-primary") / "clone"
            (cache_clone / ".git" / "rebase-merge").mkdir(parents=True, exist_ok=True)
            (cache_clone / ".git" / "rebase-merge" / "head-name").write_text("refs/heads/forge/agent/agy-primary\n", encoding="utf-8")

            # Task B from older m0 adds independent file, recovers dirty clone, rebases cleanly onto M1.
            task_b, corr_b = self._insert_task(owner, "homelab", "agy-primary")
            pub_b = str(uuid4())
            job_b = {
                "project_alias": "homelab", "agent_profile": "agy-primary",
                "task_id": str(task_b), "correlation_id": str(corr_b),
                "publication_id": pub_b, "attempt_id": str(uuid4()),
                "base_revision": m0, "workspace_digest": "c" * 64,
            }
            res_b = provider.acquire(job_b)
            snap_b = _snapshot({"README.md": "initial\n", "shared.txt": "base-line\n", "feature_b.txt": "from B\n"})
            repo_b, head_b = git_provider.prepare_source(job_b, snap_b)
            git_provider.push(job_b, repo_b, head_b, expected_remote_head=head_a, reservation=res_b, pull_requests=pr_client)
            state_b = db.agent_branch_get("homelab", "agy-primary")
            self.assertEqual(state_b["fetched_main_sha"], m1)
            self.assertEqual(state_b["sync_status"], "synced_with_main")

            # Advance main editing shared.txt -> rebase conflict must abort cleanly & record sync_status='conflict'.
            (seed / "shared.txt").write_text("main-conflicting-line\n", encoding="utf-8")
            _git(seed, "add", "shared.txt")
            _git(seed, "commit", "-m", "Main conflict")
            _git(seed, "push", "origin", "main")
            m2 = _git(seed, "rev-parse", "HEAD").stdout.strip()

            job_conflict = {
                "project_alias": "homelab", "agent_profile": "agy-primary",
                "task_id": str(task_b), "correlation_id": str(corr_b),
                "publication_id": str(uuid4()), "attempt_id": str(uuid4()),
                "base_revision": m0, "workspace_digest": "c" * 64,
            }
            snap_conflict = _snapshot({"README.md": "initial\n", "shared.txt": "agent-conflicting-line\n"})
            with self.assertRaises(work_publication_clients.SourceIntegrationConflict) as ctx:
                git_provider.prepare_source(job_conflict, snap_conflict)
            self.assertIn("shared.txt", ctx.exception.conflict_paths)
            state_conflict = db.agent_branch_get("homelab", "agy-primary")
            self.assertEqual(state_conflict["sync_status"], "conflict")
            self.assertEqual(state_conflict["fetched_main_sha"], m2)
            self.assertIn("shared.txt", state_conflict["conflict_paths"])

    def test_stage_evidence_roundtrip_and_cross_correlation_rejection(self):
        owner = f"owner-{uuid4().hex[:8]}@example.invalid"
        db = self._worker_db(owner)
        task_a, corr_a = self._insert_task(owner, "homelab", "agy-primary")
        task_b, corr_b = self._insert_task(owner, "homelab", "codex-primary")
        action_id = str(uuid4())

        raw_log = (
            b"FORGE_FIXED_CHECK_START 0\n"
            b"test tests::alpha ... FAILED\n"
            b"error[E0425]: cannot find value `x`\n"
            b"test result: FAILED. 5 passed; 1 failed; 0 ignored; 0 measured\n"
            b"FORGE_FIXED_CHECK_END 0 rc=101 elapsed_ms=900\n"
        )
        receipt = {
            "verification_profile": "forge-ci-event-v3",
            "checks": [{"index": 0, "returncode": 101}],
            "stages": [{"name": "check_0", "label": "unit tests", "elapsed_ms": 900}],
            "phase_results": [{"index": 0, "phase_id": "all-features-unit-tests", "kind": "tests"}],
        }
        extracted = work_cicd_artifacts.extract_stage_evidence_from_receipt(
            receipt, raw_log,
            project_alias="homelab", action_id=action_id, correlation_id=str(corr_a),
            task_id=str(task_a), job_name="forge-native-verify", commit_sha="a" * 40,
            branch_scope="branch/forge-agent-agy-primary-0123456789abcdef",
            pipeline_scope="branch", receipt_sha256="d" * 64, build_number=7,
            branch_name="forge/agent/agy-primary",
        )
        for ev, chunks in extracted:
            db.record_jenkins_stage_evidence(ev, chunks)

        store = work_store.WorkStore(
            owner,
            ("homelab",),
            lambda: self.cluster.connect("forge_agent"),
        )
        detail = store.get(project_alias="homelab", task_id=task_a)
        cicd = detail["correlation_cicd"]
        self.assertEqual(cicd["correlation_id"], str(corr_a))
        self.assertEqual(len(cicd["runs"]), 1)
        unit_stage = next(s for s in cicd["runs"][0]["stages"] if s["stage_id"] == "unit-tests")
        self.assertEqual(unit_stage["tests_passed"], 5)
        self.assertEqual(unit_stage["tests_failed"], 1)

        page = store.cicd_stage_log(
            project_alias="homelab", task_id=task_a, correlation_id=corr_a,
            action_id=UUID(action_id), stage_id="unit-tests", view="console",
        )
        self.assertIn("cannot find value", page["content"])
        self.assertEqual(page["tests_passed"], 5)
        self.assertEqual(page["tests_failed"], 1)

        # Verify multi-byte UTF-8 paging forward and backward lands on valid UTF-8 boundaries.
        action_utf8 = str(uuid4())
        utf8_log = ("FORGE_FIXED_CHECK_START 0\n" + ("é" * 40) + "\nFORGE_FIXED_CHECK_END 0 rc=0 elapsed_ms=10\n").encode("utf-8")
        utf8_receipt = {
            "verification_profile": "forge-ci-event-v3",
            "checks": [{"index": 0, "returncode": 0}],
            "stages": [{"name": "check_0", "label": "unit tests", "elapsed_ms": 10}],
            "phase_results": [{"index": 0, "phase_id": "all-features-unit-tests", "kind": "tests"}],
        }
        for ev, chunks in work_cicd_artifacts.extract_stage_evidence_from_receipt(
            utf8_receipt, utf8_log,
            project_alias="homelab", action_id=action_utf8, correlation_id=str(corr_a),
            task_id=str(task_a), job_name="forge-native-verify", commit_sha="a" * 40,
            branch_scope="branch/forge-agent-agy-primary-0123456789abcdef",
            pipeline_scope="branch", receipt_sha256="e" * 64, build_number=8,
            branch_name="forge/agent/agy-primary",
        ):
            db.record_jenkins_stage_evidence(ev, chunks)
        p1 = store.cicd_stage_log(
            project_alias="homelab", task_id=task_a, correlation_id=corr_a,
            action_id=UUID(action_utf8), stage_id="unit-tests", view="console", limit_bytes=15,
        )
        self.assertIsNotNone(p1["next_cursor"])
        self.assertNotIn("\ufffd", p1["content"])
        p2 = store.cicd_stage_log(
            project_alias="homelab", task_id=task_a, correlation_id=corr_a,
            action_id=UUID(action_utf8), stage_id="unit-tests", view="console",
            cursor_token=p1["next_cursor"], limit_bytes=15,
        )
        self.assertIsNotNone(p2["prev_cursor"])
        self.assertNotIn("\ufffd", p2["content"])
        p_back = store.cicd_stage_log(
            project_alias="homelab", task_id=task_a, correlation_id=corr_a,
            action_id=UUID(action_utf8), stage_id="unit-tests", view="console",
            cursor_token=p2["prev_cursor"], limit_bytes=15,
        )
        self.assertNotIn("\ufffd", p_back["content"])

        # Cross-correlation request must fail closed with 403 denied
        with self.assertRaises(work_store.Rejected) as rej:
            store.cicd_stage_log(
                project_alias="homelab", task_id=task_b, correlation_id=corr_a,
                action_id=UUID(action_id), stage_id="unit-tests", view="console",
            )
        self.assertEqual(rej.exception.status, 403)

    def test_sql_privilege_and_constraint_enforcement(self):
        # Runtime forge_agent role must be SELECT-only on M39 tables.
        with closing(self.cluster.connect("forge_agent")) as conn:
            with conn.cursor() as cur:
                for stmt in (
                    "INSERT INTO forge_agent.agent_branch_state(owner_id, project_alias, agent_profile, branch_name, branch_scope) VALUES ('o', 'p', 'a', 'forge/agent/a', 'agent-a')",
                    "DELETE FROM forge_agent.jenkins_stage_results",
                    "DELETE FROM forge_agent.jenkins_stage_log_chunks",
                ):
                    with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                        cur.execute(stmt)
                    conn.rollback()


if __name__ == "__main__":
    unittest.main()
