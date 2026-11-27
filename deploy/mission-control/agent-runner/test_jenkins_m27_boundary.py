"""Structural source boundaries for the Jenkins M27 reference port and its overlays.

Each reviewed context below is the declared scope of one change: which paths it
could touch and which single migration, if any, it introduced. The tests check
those scopes, the strict verifier closure, installer publication, the presence
of the legacy authority guards, and that applied migrations are append-only.

Earlier revisions pinned SHA-256 digests of whole source files and AST dumps.
Any later edit to a shared file (AGENT_WORK.md, setup-runner.sh, work_store.py)
broke hundreds of them without crossing a boundary, so they were retired with
the phase 0d digest removal. Behavior stays covered by the functional and
PostgreSQL regressions. These checks grant no approval, CI or admission authority.
"""

import ast
from pathlib import Path
import re
import shlex
import subprocess
import unittest


REPO_ROOT = Path(__file__).resolve().parents[3]
MIGRATIONS = REPO_ROOT / "deploy/mission-control/sql/migrations"
MANAGEMENT = REPO_ROOT / "deploy/mission-control/sql/management"
RUNNER = Path(__file__).resolve().parent

# Live schema version and a revision whose applied migrations and fragments
# are the deployed bytes. Applied schema is append-only (see
# test_applied_migrations_and_fragments_are_append_only).
APPLIED_SCHEMA_VERSION = 40
APPLIED_SCHEMA_REVISION = 'b4b1def235c4fe3d56243b3bfdd07e82fb11b911'

# Applied migrations 0001-0026 (the M27 baseline).
MIGRATION_PATHS = frozenset({
    '0001_agent.sql',
    '0002_ide_workspace.sql',
    '0003_ide_chat_history.sql',
    '0004_ide_chat_default_expiration.sql',
    '0005_inference_dispatch.sql',
    '0006_inference_model_selection.sql',
    '0007_ide_mail.sql',
    '0008_mail_archive.sql',
    '0009_coding_admission.sql',
    '0010_coding_execution_and_usage.sql',
    '0011_coding_elevated_tool_policy.sql',
    '0012_coding_progress.sql',
    '0013_coding_task_session_policy.sql',
    '0014_coding_plan_workflow.sql',
    '0015_coding_feedback_loop.sql',
    '0016_coding_autonomous_workflows.sql',
    '0017_coding_codex_warm_sessions.sql',
    '0018_coding_grok_provider.sql',
    '0019_coding_muse_provider.sql',
    '0020_external_agent_transport.sql',
    '0021_coding_event_trace.sql',
    '0022_external_workflows.sql',
    '0023_coding_agy_unattended.sql',
    '0024_native_jenkins_context.sql',
    '0025_native_delivery_repair.sql',
    '0026_coding_owner_continuation.sql',
})

UNCHANGED_BASELINE_PATHS = frozenset({
    'deploy/mission-control/sql/management/verify-owner-continuation.sql',
    'deploy/mission-control/sql/management/verify-feedback.sql',
    'deploy/mission-control/sql/management/verify-native-delivery.sql',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/agent-runner/work_publication_clients.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_native_repair.py',
    'deploy/mission-control/agent-runner/work_owner_reverification.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_policy.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'src/rust/forge-ide-ui/tests/work_review.rs',
    'deploy/mission-control/agent-runner/test_work_owner_followup.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/test_ci_supervision.py',
    'deploy/mission-control/agent-runner/test_work_branch_first.py',
    'deploy/mission-control/agent-runner/test_work_external_delivery.py',
    'deploy/mission-control/agent-runner/test_external_workflow_branch_scope.py',
    'deploy/mission-control/agent-runner/test_external_flow.py',
    'deploy/mission-control/agent-runner/test_work_native_jenkins.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/jenkins/clear-ci-cache.py',
    'deploy/mission-control/jenkins/setup-ci.py',
    'docs/reports/forge-cargo-cache-qualification-20261121.md',
    'docs/reports/forge-cargo-cache-qualification-20261121.json',
    'deploy/mission-control/jenkins/README.md',
    'docs/reports/forge-cargo-cache-publication-correctness-20261122.md',
    'docs/reports/forge-cargo-cache-publication-correctness-20261122.json',
})

# Exact included context bytes; these files intentionally differ from4dc.
REVIEWER_CONTEXT_REVISION = "2df4908da25eb7a737678ce21cecc2ce0baf5428"
INCLUDED_REVIEWER_CONTEXT_PATHS = frozenset({
    'work_owner_control.py',
    'work_workflow.py',
    'work_reviewer_correction.py',
    'work_peer_review.py',
    'work_executor.py',
    'test_work_peer_review.py',
    'test_work_reviewer_correction.py',
})

# Reviewed CI contract correction 7745032, integrated on 23 November 2026.
# Preserve the historical v1 baseline above as provenance. These exact reviewed
# source bytes add scope-aware v2 while preserving v1 receipts and full checks;
# regression tests cover producer/consumer agreement, policy/scope and skip fences.
# All historical SQL and legacy authority AST guards remain independently pinned.
# Source qualification does not grant deployment, CI success or merge authority.
REVIEWED_CI_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_native_repair.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/test_ci_supervision.py',
    'deploy/mission-control/agent-runner/test_work_external_delivery.py',
    'deploy/mission-control/agent-runner/test_work_native_jenkins.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/jenkins/setup-ci.py',
    'deploy/mission-control/jenkins/README.md',
    'deploy/mission-control/sql/management/verify-native-delivery.sql',
})

# Python3.12 AST serialization; no Git or runtime imports are needed.
UNCHANGED_LEGACY_GUARDS = frozenset({
    'work_workflow.py::BoundDelegatedAuthority.validate',
    'work_workflow.py::WorkflowStore._code_handoff',
    'work_owner_control.py::owner_continuation_used',
    'work_owner_control.py::_owner_source_history',
    'work_owner_control.py::_followup_replay',
    'work_owner_control.py::require_owner_capability',
    'work_owner_control.py::NativeOwnerControl.review_decide',
    'work_store.py::WorkStore.plan_decide',
    'work_store.py::WorkStore.review_decide',
    'work_store.py::WorkStore._append_review_retry',
    'work_store.py::WorkStore._publication_busy',
    'work_store.py::WorkStore._require_transition_authority',
    'work_workflow.py::BoundNativeRetryAuthority',
    'work_workflow.py::BoundPreparationRetryAuthority',
    'work_workflow.py::WorkflowStore.retry_native_timeout',
    'work_workflow.py::WorkflowStore.retry_peer_verdict',
    'work_workflow.py::WorkflowStore.retry_preparation',
    'work_workflow.py::WorkflowStore._code_review_source',
    'work_workflow.py::WorkflowStore._exhausted_code_budget',
    'work_peer_review.py::parse_review_verdict',
})

# Explicit task-organization source qualification, based on combined351e.
# These selected files intentionally differ from both4dc and reviewer2df.
TASK_ORGANIZATION_CONTEXT_PATHS = frozenset({
    'src/rust/forge-ide-web-host/tests/consumer/work.rs',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'src/rust/forge-ide-ui/src/protocol/work.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.test.mjs',
})

# Retirement feature origin is retained below. Its source fingerprints are
# qualified against main370bcffd6d4c71d83ba0fa8742257713da8cf76d after the
# reviewed module formalization, forge-core imports and additive contract tests.
# Those structural changes preserve dormant Chat/Mail data and retired routing.
# Historical migrations and authority AST guards remain independently pinned.
# These fingerprints qualify source only; they confer no CI or merge authority.
RETIREMENT_CONTEXT_REVISION = "243742283dc1a182738c2d4ecf131618b1dc7b8e"
INCLUDED_RETIREMENT_CONTEXT_PATHS = frozenset({
    'src/rust/forge-ide-ui/src/lib.rs',
    'src/rust/forge-ide-ui/src/model/bookmark.rs',
    'src/rust/forge-ide-ui/src/model/edit.rs',
    'src/rust/forge-ide-ui/src/model/projection.rs',
    'src/rust/forge-ide-ui/src/model/source.rs',
    'src/rust/forge-ide-ui/src/protocol/legacy.rs',
    'src/rust/forge-ide-ui/src/view.rs',
    'src/rust/forge-ide-ui/tests/chat_retirement.rs',
    'src/rust/forge-ide-ui/tests/mail_archive.rs',
    'src/rust/forge-ide-ui/tests/model.rs',
    'src/rust/forge-ide-ui/tests/wasm_abi.mjs',
    'src/rust/forge-ide-ui/tests/worker_glue.mjs',
    'src/rust/forge-ide-web-host/README.md',
    'src/rust/forge-ide-web-host/src/assets.rs',
    'src/rust/forge-ide-web-host/src/chat.rs',
    'src/rust/forge-ide-web-host/src/http.rs',
    'src/rust/forge-ide-web-host/src/lib.rs',
    'src/rust/forge-ide-web-host/src/service.rs',
    'src/rust/forge-ide-web-host/src/service/edit_write.rs',
    'src/rust/forge-ide-web-host/src/session.rs',
    'src/rust/forge-ide-web-host/src/workspace.rs',
    'src/rust/forge-ide-web-host/tests/consumer.rs',
    'src/rust/forge-ide-web/README.md',
    'src/rust/forge-ide-web/src/generate/javascript.rs',
    'src/rust/forge-ide-web/src/generate/mod.rs',
    'src/rust/forge-ide-web/tests/generation.rs',
    'src/rust/forge-ide-web/tests/playtest-wire.mjs',
})

CONSOLIDATED_UI_CONTEXT_PATHS = frozenset({
    'src/rust/forge-ide-ui/src/model.rs',
    'src/rust/forge-ide-ui/src/model/agents.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs',
    'src/rust/forge-ide-ui/src/protocol.rs',
    'src/rust/forge-ide-web/src/generate/application.rs',
    'src/rust/forge-ide-web/src/wasm.rs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
    'src/rust/forge-ide-web/ui/styles.yaml',
})

RETIRED_BROWSER_SOURCE_FILES = ('src/rust/forge-ide-ui/src/model/chat.rs', 'src/rust/forge-ide-ui/src/model/chat/history.rs', 'src/rust/forge-ide-ui/src/model/chat/models.rs', 'src/rust/forge-ide-ui/src/model/chat/run.rs', 'src/rust/forge-ide-ui/src/model/mail.rs', 'src/rust/forge-ide-ui/src/presentation.rs', 'src/rust/forge-ide-ui/src/protocol/chat_run.rs', 'src/rust/forge-ide-ui/src/protocol/mail.rs', 'src/rust/forge-ide-web-host/src/edit.rs', 'src/rust/forge-ide-web-host/src/history.rs', 'src/rust/forge-ide-web-host/src/mail_dispatch.rs', 'src/rust/forge-ide-web-host/src/run_dispatch.rs')

# Reviewed additive M28 coordinator context on main aae81e4. Historical raw
# maps below remain literal provenance; only these explicit current paths change.
# Functional/privilege tests and unchanged legacy AST guards remain mandatory.
POST_MERGE_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/test_work_post_merge.py',
    'deploy/mission-control/agent-runner/test_work_post_merge_provider.py',
    'deploy/mission-control/agent-runner/test_work_post_merge_store.py',
    'deploy/mission-control/agent-runner/work_post_merge.py',
    'deploy/mission-control/agent-runner/work_post_merge_provider.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/jenkins/README.md',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/management/verify-post-merge.sql',
    'deploy/mission-control/sql/migrations/0028_post_merge_verification.sql',
})

# Reviewed ABI14 delivery projection in f246b1494f29cf86af8a32a24da80d56c17427d1.
# Keep all earlier retirement/task-organization/UI hashes as literal provenance.
# This narrow current overlay binds the negotiated DTO/header, typed rendering,
# atomic ABI metadata and their tests/docs. It grants no live activation or CI
# success and changes no SQL, native publication guard or backend source pin.
UI_DELIVERY_CONTEXT_REVISION = "f246b1494f29cf86af8a32a24da80d56c17427d1"
UI_DELIVERY_CONTEXT_PATHS = frozenset({
    '.github/workflows/ide-ui-browser.yml',
    'docs/architecture/forge-post-merge-verification.md',
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-ui/README.md',
    'src/rust/forge-ide-ui/src/model/agents.rs',
    'src/rust/forge-ide-ui/src/model/agents/delivery.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests/delivery.rs',
    'src/rust/forge-ide-ui/src/model/agents/workflow.rs',
    'src/rust/forge-ide-ui/src/protocol/work.rs',
    'src/rust/forge-ide-ui/src/protocol/work/delivery.rs',
    'src/rust/forge-ide-ui/tests/mail_archive.rs',
    'src/rust/forge-ide-ui/tests/wasm_abi.mjs',
    'src/rust/forge-ide-web-host/README.md',
    'src/rust/forge-ide-web-host/deploy/README.md',
    'src/rust/forge-ide-web-host/src/assets.rs',
    'src/rust/forge-ide-web-host/src/chat.rs',
    'src/rust/forge-ide-web-host/src/design/agent_autonomy.rs',
    'src/rust/forge-ide-web-host/tests/consumer.rs',
    'src/rust/forge-ide-web-host/tests/consumer/work.rs',
    'src/rust/forge-ide-web/README.md',
    'src/rust/forge-ide-web/WORKBENCH.md',
    'src/rust/forge-ide-web/src/generate/application.rs',
    'src/rust/forge-ide-web/src/generate/application/delivery.rs',
    'src/rust/forge-ide-web/src/generate/javascript.rs',
    'src/rust/forge-ide-web/src/generate/mod.rs',
    'src/rust/forge-ide-web/src/wasm.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.test.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
    'src/rust/forge-ide-web/tests/generation.rs',
    'src/rust/forge-ide-web/tests/playtest-wire.mjs',
    'src/rust/forge-ide-web/ui/styles.yaml',
})


# Current additive delivery source; historical maps above remain provenance.
DELIVERY_AUTOMATION_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/runner.py',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_ci_supervision.py',
    'deploy/mission-control/agent-runner/test_delivery_ops.py',
    'deploy/mission-control/agent-runner/test_delivery_worker.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_portal_release_actor.py',
    'deploy/mission-control/agent-runner/test_portal_release_recovery.py',
    'deploy/mission-control/agent-runner/test_portal_release_startup.py',
    'deploy/mission-control/agent-runner/test_work_context_recovery.py',
    'deploy/mission-control/agent-runner/test_work_delivery_automation.py',
    'deploy/mission-control/agent-runner/test_work_feedback_delivery.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/test_work_post_merge_store.py',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_context_recovery.py',
    'deploy/mission-control/agent-runner/work_context_recovery_contract.py',
    'deploy/mission-control/agent-runner/work_delivery_ops.py',
    'deploy/mission-control/agent-runner/work_delivery_repair.py',
    'deploy/mission-control/agent-runner/work_delivery_worker.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_review_context.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_worker.py',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/backend-suites.py',
    'deploy/mission-control/jenkins/setup-ci.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/portal_release_actor.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/grant-muse-read-access.sql',
    'deploy/mission-control/sql/management/grant-runner.sql',
    'deploy/mission-control/sql/management/verify-delivery.sql',
    'deploy/mission-control/sql/management/verify-muse-read-access.sql',
    'deploy/mission-control/sql/management/verify-post-merge.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0029_delivery_automation.sql',
    'docs/architecture/forge-post-merge-verification.md',
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-ui/src/model/agents/tests/delivery.rs',
    'src/rust/forge-ide-ui/src/protocol/work/delivery.rs',
    'src/rust/forge-ide-web-host/tests/consumer/work.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.test.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
})


# Reviewed agent-wide retry overlay. Historical source and authority pins above
# remain immutable provenance. Only numeric allowance reads change in the three
# guarded methods below; roles, grants, recipes, source and replay checks remain.
# These fingerprints qualify source; they confer no CI or deployment authority.
AGENT_WIDE_RETRY_CONTEXT_PATHS = frozenset({
    'src/rust/forge-ide-ui/tests/work_review.rs',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_external_flow.py',
    'deploy/mission-control/agent-runner/test_work_owner_followup.py',
    'deploy/mission-control/agent-runner/test_work_peer_review.py',
    'deploy/mission-control/agent-runner/test_work_reviewer_correction.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_context_recovery.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_owner_control.py',
    'deploy/mission-control/agent-runner/work_owner_reverification.py',
    'deploy/mission-control/agent-runner/work_peer_review.py',
    'deploy/mission-control/agent-runner/work_retry_continuation.py',
    'deploy/mission-control/agent-runner/work_retry_policy.py',
    'deploy/mission-control/agent-runner/work_reviewer_correction.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/backend-suites.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/verify-agent-retries.sql',
    'deploy/mission-control/sql/management/verify-feedback.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0030_agent_wide_retry_policy.sql',
    'docs/architecture/forge-post-merge-verification.md',
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-ui/src/protocol/work.rs',
    'src/rust/forge-ide-ui/src/protocol/work/delivery.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
})

AGENT_WIDE_RETRY_GUARDS = frozenset({
    'work_owner_control.py::NativeOwnerControl.review_decide',
    'work_workflow.py::WorkflowStore.retry_peer_verdict',
    'work_workflow.py::WorkflowStore.retry_preparation',
})

# Reviewed per-event retry context. Task totals are audit history; immutable
# predecessor source/guard maps remain provenance. These source identities do
# not grant CI success, publication or privileged deployment authority.
EVENT_RETRY_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/test_work_reviewer_correction_events.py',
    'deploy/mission-control/agent-runner/test_work_delivery_automation.py',
    'deploy/mission-control/jenkins/setup-ci.py',
    'deploy/mission-control/agent-runner/test_ci_installer_v3.py',
    'deploy/mission-control/agent-runner/runner.py',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_ci_supervision.py',
    'deploy/mission-control/agent-runner/test_external_flow.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_portal_release_actor.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_peer_review.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_ci_phases.py',
    'deploy/mission-control/agent-runner/work_context_recovery.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_peer_review.py',
    'deploy/mission-control/agent-runner/work_post_merge.py',
    'deploy/mission-control/agent-runner/work_post_merge_provider.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_retry_continuation.py',
    'deploy/mission-control/agent-runner/work_retry_events.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_reviewer_correction.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/portal_release_actor.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/fragments/0031_ci_stage_receipts.sql',
    'deploy/mission-control/sql/fragments/0031_delivery_retry_events.sql',
    'deploy/mission-control/sql/fragments/0031_native_retry_events.sql',
    'deploy/mission-control/sql/fragments/0031_retry_event_fingerprints.sql',
    'deploy/mission-control/sql/management/verify-agent-retries.sql',
    'deploy/mission-control/sql/management/verify-delivery.sql',
    'deploy/mission-control/sql/management/verify-feedback.sql',
    'deploy/mission-control/sql/management/verify-owner-continuation.sql',
    'deploy/mission-control/sql/management/verify-post-merge.sql',
    'deploy/mission-control/sql/management/verify-retry-events.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0031_retry_events.sql',
    'docs/architecture/forge-post-merge-verification.md',
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-ui/src/model/agents.rs',
    'src/rust/forge-ide-ui/src/model/agents/delivery.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests/delivery.rs',
    'src/rust/forge-ide-ui/src/model/agents/workflow.rs',
    'src/rust/forge-ide-ui/src/protocol/work.rs',
    'src/rust/forge-ide-ui/src/protocol/work/delivery.rs',
    'src/rust/forge-ide-ui/tests/wasm_abi.mjs',
    'src/rust/forge-ide-ui/tests/work_review.rs',
    'src/rust/forge-ide-web-host/src/assets.rs',
    'src/rust/forge-ide-web-host/src/chat.rs',
    'src/rust/forge-ide-web-host/tests/consumer.rs',
    'src/rust/forge-ide-web-host/tests/consumer/work.rs',
    'src/rust/forge-ide-web/src/generate/application.rs',
    'src/rust/forge-ide-web/src/generate/javascript.rs',
    'src/rust/forge-ide-web/src/generate/mod.rs',
    'src/rust/forge-ide-web/src/wasm.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
    'src/rust/forge-ide-web/tests/generation.rs',
    'src/rust/forge-ide-web/tests/playtest-wire.mjs',
    'deploy/mission-control/agent-runner/work_events.py',
    'deploy/mission-control/agent-runner/work_jenkins_lifecycle.py',
    'deploy/mission-control/agent-runner/test_work_jenkins_lifecycle.py',
    'deploy/mission-control/agent-runner/test_work_event_attribution.py',
    'deploy/mission-control/agent-runner/test_work_delivery_contract.py',
    'deploy/mission-control/agent-runner/fixtures/peer-target-351b8c41.json',
    'deploy/mission-control/agent-runner/fixtures/peer-verdict-719aa2b2.json',
    'deploy/mission-control/sql/management/verify-autonomy.sql',
    'deploy/mission-control/sql/management/verify-external-agent.sql',
    'deploy/mission-control/sql/management/verify-external-workflows.sql',
    'deploy/mission-control/sql/management/verify-jenkins-events.sql',
    'deploy/mission-control/sql/management/verify-task-events.sql',
    'deploy/mission-control/sql/management/verify-agy-unattended.sql',
})

EVENT_RETRY_GUARDS = frozenset({
    'work_workflow.py::WorkflowStore.retry_peer_verdict',
})

# Exact additive M32 source; predecessor maps remain historical provenance.
# Source identities never substitute for real CI or deployment receipts.
LINEAGE_RETRY_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_events.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/fragments/0032_observer_retry_lineage.sql',
    'deploy/mission-control/sql/management/verify-delivery-lineage.sql',
    'deploy/mission-control/sql/management/verify-delivery.sql',
    'deploy/mission-control/sql/management/verify-retry-events.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0032_observer_retry_lineage.sql',
    'docs/architecture/ci-stage-evidence.md',
})

# Exact additive M33 source; every predecessor pin remains historical authority.
SUPERSEDING_REVIEW_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
    'deploy/mission-control/agent-runner/test_work_review_transcription.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_superseding_review.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/fragments/0033_superseding_reviews.sql',
    'deploy/mission-control/sql/management/verify-autonomy.sql',
    'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0033_superseding_reviews.sql',
    'docs/architecture/proposals/forge-autonomous-agent-workflow.md',
})

# Exact peer-feedback correction; all applied SQL and predecessor pins remain immutable.
PEER_FEEDBACK_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py',
    'deploy/mission-control/agent-runner/test_work_linked_child_feedback.py',
    'deploy/mission-control/agent-runner/test_work_peer_context.py',
    'deploy/mission-control/agent-runner/test_work_peer_feedback_recovery.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_context_recovery.py',
    'deploy/mission-control/agent-runner/work_peer_context.py',
    'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'docs/architecture/proposals/forge-autonomous-agent-workflow.md',
})

# Exact reviewer protocol and transport fix; predecessor authority and applied SQL stay immutable.
REVIEW_PROTOCOL_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/README.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py',
    'deploy/mission-control/agent-runner/test_external_transport_recovery.py',
    'deploy/mission-control/agent-runner/test_work_cli_transport.py',
    'deploy/mission-control/agent-runner/test_work_review_revision_attestations.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_cli_transport.py',
    'deploy/mission-control/agent-runner/work_executor.py',
    'deploy/mission-control/agent-runner/work_external_recovery.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_superseding_review.py',
    'deploy/mission-control/agent-runner/work_worker.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'docs/architecture/external-agent-workflow.md',
})

# Current additive ABI15 event-stream stage-diagram UI source; historical maps
# above remain literal provenance. This narrow overlay binds only the UI model,
# declarative component/emitters, atomic ABI15 metadata, docs, and UI/host tests.
# It grants no live activation or CI success and changes no SQL, native
# publication guard, or backend source pin.
UI_STAGE_DIAGRAM_CONTEXT_PATHS = frozenset({
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-ui/README.md',
    'src/rust/forge-ide-ui/src/model/agents.rs',
    'src/rust/forge-ide-ui/src/model/agents/history.rs',
    'src/rust/forge-ide-ui/src/model/agents/stage_diagram.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests/stage_diagram.rs',
    'src/rust/forge-ide-ui/src/model/projection.rs',
    'src/rust/forge-ide-ui/src/view.rs',
    'src/rust/forge-ide-ui/tests/worker_glue.mjs',
    'src/rust/forge-ide-web-host/README.md',
    'src/rust/forge-ide-web-host/deploy/README.md',
    'src/rust/forge-ide-web/README.md',
    'src/rust/forge-ide-web/WORKBENCH.md',
    'src/rust/forge-ide-web/src/generate/application.rs',
    'src/rust/forge-ide-web/src/generate/component.rs',
    'src/rust/forge-ide-web/src/generate/html.rs',
    'src/rust/forge-ide-web/src/generate/javascript.rs',
    'src/rust/forge-ide-web/src/wasm.rs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
    'src/rust/forge-ide-web/tests/generation.rs',
    'src/rust/forge-ide-web/tests/input-presentation.mjs',
    'src/rust/forge-ide-web/ui/styles.yaml',
})



# Current additive Jenkins pipeline stages diagram source; historical maps
# above remain provenance. Binds the stage_started migration, the lifecycle
# producer and drain, the stage emitter, the server and UI projections, the
# ABI 14->15 move, and their tests and browser journey. It grants no live
# activation and changes no legacy owner/publication guard.
PIPELINE_DIAGRAM_CONTEXT_PATHS = frozenset({
    'docs/design/implementation-status.md',
    'deploy/mission-control/agent-runner/jenkins_api.py',
    'deploy/mission-control/agent-runner/jenkins_events.py',
    'deploy/mission-control/agent-runner/runner.py',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_ci_supervision.py',
    'deploy/mission-control/agent-runner/test_jenkins_api.py',
    'deploy/mission-control/agent-runner/test_jenkins_codec.py',
    'deploy/mission-control/agent-runner/test_jenkins_pipeline_lifecycle.py',
    'deploy/mission-control/agent-runner/test_jenkins_stage_emitter.py',
    'deploy/mission-control/agent-runner/test_jenkins_store.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_pipeline_projection.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_native_jenkins.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_jenkins_lifecycle.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_pipeline_projection.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/jenkins/setup-ci.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/verify-jenkins-events.sql',
    'deploy/mission-control/sql/migrations/0034_jenkins_stage_started.sql',
    'src/rust/forge-ide-ui/src/model/agents.rs',
    'src/rust/forge-ide-ui/src/model/agents/pipeline_diagram.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs',
    'src/rust/forge-ide-ui/src/model/projection.rs',
    'src/rust/forge-ide-ui/src/protocol/work.rs',
    'src/rust/forge-ide-ui/src/view.rs',
    'src/rust/forge-ide-ui/tests/wasm_abi.mjs',
    'src/rust/forge-ide-web-host/src/assets.rs',
    'src/rust/forge-ide-web-host/tests/consumer.rs',
    'src/rust/forge-ide-web/README.md',
    'src/rust/forge-ide-web/WORKBENCH.md',
    'src/rust/forge-ide-web/src/generate/application.rs',
    'src/rust/forge-ide-web/src/generate/component.rs',
    'src/rust/forge-ide-web/src/generate/html.rs',
    'src/rust/forge-ide-web/src/generate/javascript.rs',
    'src/rust/forge-ide-web/src/generate/mod.rs',
    'src/rust/forge-ide-web/src/wasm.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
    'src/rust/forge-ide-web/tests/generation.rs',
    'src/rust/forge-ide-web/tests/playtest-wire.mjs',
    'src/rust/forge-ide-web/ui/styles.yaml',
})


# Reviewed branch-only compatibility correction. Historical maps remain literal
# provenance; only these explicit current bytes may supersede their source pins.
# M35 has not shipped on main. No applied migration or guard is qualified here.
MUSE_MERGE_COMPATIBILITY_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
    'deploy/mission-control/agent-runner/test_work_peer_diagnostics_schema.py',
    'deploy/mission-control/agent-runner/test_work_peer_replay.py',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/sql/management/verify-autonomy.sql',
    'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
    'deploy/mission-control/sql/migrations/0035_event_stream_peer_review_diagnostics.sql',
})


# Exact additive M38 external plan approve-override source; all applied SQL
# migrations (0001..0037) and predecessor maps remain immutable provenance.
# Source identities never substitute for real CI or deployment receipts.
PLAN_APPROVE_OVERRIDE_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
    'deploy/mission-control/agent-runner/test_external_plan_override.py',
    'deploy/mission-control/agent-runner/test_external_plan_override_cli.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_external.py',
    'deploy/mission-control/agent-runner/work_external_plan_override.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/grant-external-workflows.sql',
    'deploy/mission-control/sql/management/verify-external-workflows.sql',
    'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql',
    'docs/architecture/external-agent-workflow.md',
    'docs/architecture/proposals/external-plan-approve-override.md',
})

# Exact additive M39 correlation CI/CD stages, persistent per-agent cached
# branches with exclusive active-workflow ownership, and ABI 16 stage inspector
# source overlay. All applied migrations (0001..0038), predecessor provenance
# maps, and legacy owner/publication AST guards remain unchanged.
CICD_STAGES_BRANCH_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/README.md',
    'deploy/mission-control/agent-runner/jenkins_api.py',
    'deploy/mission-control/agent-runner/jenkins_events.py',
    'deploy/mission-control/agent-runner/jenkins_store.py',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agent_branch_ownership.py',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
    'deploy/mission-control/agent-runner/test_work_event_attribution.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
    'deploy/mission-control/agent-runner/work_agent_branch.py',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_cicd_artifacts.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_external_delivery_store.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_jenkins_lifecycle.py',
    'deploy/mission-control/agent-runner/work_merge.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_pipeline_projection.py',
    'deploy/mission-control/agent-runner/work_post_merge.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_publication_clients.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_worker_config.py',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/jenkins/README.md',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/grant-muse-read-access.sql',
    'deploy/mission-control/sql/management/grant-runner.sql',
    'deploy/mission-control/sql/management/verify-cicd-stages.sql',
    'deploy/mission-control/sql/management/verify-jenkins-events.sql',
    'deploy/mission-control/sql/management/verify-muse-read-access.sql',
    'deploy/mission-control/sql/management/verify-runner-privileges.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0041_agent_branch_and_cicd_stages.sql',
    'docs/architecture/ci-pipeline.md',
    'docs/architecture/external-agent-workflow.md',
    'docs/architecture/forge-jenkins.md',
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-ui/src/model.rs',
    'src/rust/forge-ide-ui/src/model/agents.rs',
    'src/rust/forge-ide-ui/src/model/agents/cicd.rs',
    'src/rust/forge-ide-ui/src/model/agents/pipeline_diagram.rs',
    'src/rust/forge-ide-ui/src/model/agents/stage_diagram.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs',
    'src/rust/forge-ide-ui/src/model/agents/tests/workflow.rs',
    'src/rust/forge-ide-ui/src/protocol.rs',
    'src/rust/forge-ide-ui/src/protocol/work.rs',
    'src/rust/forge-ide-ui/src/protocol/work/cicd.rs',
    'src/rust/forge-ide-ui/src/view.rs',
    'src/rust/forge-ide-ui/tests/mail_archive.rs',
    'src/rust/forge-ide-ui/tests/wasm_abi.mjs',
    'src/rust/forge-ide-ui/tests/worker_glue.mjs',
    'src/rust/forge-ide-web-host/src/work.rs',
    'src/rust/forge-ide-web-host/tests/consumer/work.rs',
    'src/rust/forge-ide-web/README.md',
    'src/rust/forge-ide-web/src/generate/application.rs',
    'src/rust/forge-ide-web/src/generate/application/cicd.rs',
    'src/rust/forge-ide-web/src/generate/javascript.rs',
    'src/rust/forge-ide-web/src/generate/mod.rs',
    'src/rust/forge-ide-web/src/wasm.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
    'src/rust/forge-ide-web/tests/generation.rs',
    'src/rust/forge-ide-web/tests/playtest-wire.mjs',
    'src/rust/forge-ide-web/ui/styles.yaml',
})


# Audited task-recovery source. Prior maps remain historical provenance.
# Applied SQL and other legacy ownership guards are never qualified here.
TASK_RECOVERY_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/test_external_code_context_recovery.py',
    'deploy/mission-control/agent-runner/test_external_diff_contract.py',
    'deploy/mission-control/agent-runner/test_native_deadline_events.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_external_recovery.py',
    'deploy/mission-control/agent-runner/work_external_review_context.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'src/rust/forge-ide-ui/src/model/agents/tests/scopes.rs',
    'src/rust/forge-ide-ui/src/model/agents/workflow.rs',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
    'src/rust/forge-ide-web/tests/browser/journey.mjs',
})

TASK_RECOVERY_GUARDS = frozenset({
    'work_workflow.py::WorkflowStore.retry_native_timeout',
})


# Already accepted on main57a0aca: exact M36 legacy-progress grandfathering
# verifier bytes. This qualifies no applied migration or other SQL authority.
M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/management/verify-task-events.sql',
})


# Jenkins observation capability, separately selected from frozen mutations.
# Applied SQL, existing mutation configuration, and all prior literal maps stay
# unchanged. These are exact reviewed source bytes, never CI proof substitutes.
JENKINS_PAGED_OBSERVER_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_jenkins_read_observer.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_jenkins_observer.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'src/rust/forge-jenkins/README.md',
    'src/rust/forge-jenkins/src/client.rs',
    'src/rust/forge-jenkins/src/lib.rs',
    'src/rust/forge-jenkins/src/observation.rs',
    'src/rust/forge-jenkins/tests/client.rs',
    'src/rust/forge-local/README.md',
    'src/rust/forge-local/src/application.rs',
    'src/rust/forge-local/src/jenkins.rs',
    'src/rust/forge-local/tests/application.rs',
    'src/rust/forge-local/tests/jenkins_application.rs',
    'src/rust/forge-terminal/README.md',
    'src/rust/forge-terminal/src/jenkins.rs',
    'src/rust/forge-terminal/src/parser.rs',
    'src/rust/forge-terminal/tests/jenkins.rs',
})


# Exact verification-only context registration; no SQL, gate contract,
# existing mutation transport, or historical source pin is changed.
NATIVE_CI_CONTEXT_RECOVERY_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_native_ci_reprepare.py',
    'deploy/mission-control/agent-runner/work_native_ci_reprepare.py',
})


# Audited opt-in content identity source; no applied SQL or legacy authority change.
CONTENT_IDENTITY_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_external_content_identity.py',
    'deploy/mission-control/agent-runner/test_native_content_identity.py',
    'deploy/mission-control/agent-runner/test_work_artifact_identity.py',
    'deploy/mission-control/agent-runner/test_work_identity_store.py',
    'deploy/mission-control/agent-runner/work_artifact_identity.py',
    'deploy/mission-control/agent-runner/work_artifact_source.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_external_identity.py',
    'deploy/mission-control/agent-runner/work_external_review_context.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_identity_contract.py',
    'deploy/mission-control/agent-runner/work_identity_store.py',
    'deploy/mission-control/agent-runner/work_native_identity.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_worker.py',
    'docs/README.md',
    'docs/architecture/README.md',
    'docs/architecture/content-identity.md',
    'docs/architecture/diagrams/mermaid/README.md',
    'docs/architecture/diagrams/mermaid/content-identity-components.mmd',
    'docs/architecture/diagrams/mermaid/content-identity-data.mmd',
    'docs/architecture/diagrams/mermaid/content-identity-external.mmd',
    'docs/architecture/diagrams/mermaid/content-identity-native.mmd',
    'docs/architecture/diagrams/mermaid/images/content-identity-components.svg',
    'docs/architecture/diagrams/mermaid/images/content-identity-data.svg',
    'docs/architecture/diagrams/mermaid/images/content-identity-external.svg',
    'docs/architecture/diagrams/mermaid/images/content-identity-native.svg',
    'docs/architecture/diagrams/mermaid/render-manifest.json',
    'docs/architecture/diagrams/mermaid/render.py',
    'docs/architecture/diagrams/mermaid/validate.mjs',
    'docs/design/implementation-status.md',
    'src/rust/forge-ide-web-host/build.rs',
    'src/rust/forge-ide-web-host/src/assets.rs',
    'src/rust/forge-ide-web-host/src/design/content_identity.rs',
    'src/rust/forge-ide-web-host/src/design/landing.rs',
    'src/rust/forge-ide-web-host/src/design/mod.rs',
    'src/rust/forge-ide-web-host/tests/consumer.rs',
})


# Private matched-executor recovery; preserve every historical context and SQL.
NATIVE_POSTFIX_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_native_ci_postfix.py',
    'deploy/mission-control/agent-runner/test_native_ci_postfix_proof.py',
    'deploy/mission-control/agent-runner/work_identity_contract.py',
    'deploy/mission-control/agent-runner/work_native_ci_postfix.py',
    'deploy/mission-control/agent-runner/work_native_ci_postfix_proof.py',
})


# Scoped runner/verifier policy-parity and gate-manifest mismatch diagnostics;
# preserves all applied SQL, protected gate paths, and legacy authority AST pins.
VERIFIER_POLICY_PARITY_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_verifier_policy_parity.py',
    'deploy/mission-control/agent-runner/test_work_artifact_identity.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_ci_phases.py',
    'deploy/mission-control/agent-runner/work_delivery_repair.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
})


# Scoped terminal workflow/retry projection settlement (M39); preserves all
# applied M01-M38 SQL, protected gate paths, and legacy authority AST pins.
TERMINAL_PROJECTION_SETTLEMENT_CONTEXT_PATHS = frozenset({
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_native_ci_postfix.py',
    'deploy/mission-control/agent-runner/test_native_deadline_events.py',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
    'deploy/mission-control/agent-runner/test_terminal_projection_settlement.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_retry_events.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/grant-runner.sql',
    'deploy/mission-control/sql/management/repair-stale-terminal-projections.sql',
    'deploy/mission-control/sql/migrations/0039_terminal_retry_settlement.sql',
    'src/rust/forge-ide-ui/src/model/agents/tests/workflow.rs',
})


# Explicit event-stream stabilization qualification. These source bytes add
# source-bound configured-model routing, clean main integration, bounded CI
# reconciliation and complete parallel test execution. Historical maps and SQL
# remain immutable provenance; only the three scoped future-recipe selectors
# below change their guard AST. This is not approval or deployment authority.
EVENT_STREAM_STABILITY_CONTEXT_PATHS = frozenset({
    'AGENTS.md',
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agents_runtime_integration.py',
    'deploy/mission-control/agent-runner/test_backend_main_parallel.py',
    'deploy/mission-control/agent-runner/test_ci_restart_deferral.py',
    'deploy/mission-control/agent-runner/test_external_artifact_repair.py',
    'deploy/mission-control/agent-runner/test_external_content_identity.py',
    'deploy/mission-control/agent-runner/test_external_flow.py',
    'deploy/mission-control/agent-runner/test_external_main_refresh.py',
    'deploy/mission-control/agent-runner/test_grok_auth_cleanup.py',
    'deploy/mission-control/agent-runner/test_grok_trust.py',
    'deploy/mission-control/agent-runner/test_muse_auth_cleanup.py',
    'deploy/mission-control/agent-runner/test_native_ci_reconcile_stability.py',
    'deploy/mission-control/agent-runner/test_provider_authority_ci_policy.py',
    'deploy/mission-control/agent-runner/test_provider_cooldown.py',
    'deploy/mission-control/agent-runner/test_provider_model_route.py',
    'deploy/mission-control/agent-runner/test_provider_quota_audit.py',
    'deploy/mission-control/agent-runner/test_provider_quota_diagnostics.py',
    'deploy/mission-control/agent-runner/test_stopped_binary_pin.py',
    'deploy/mission-control/agent-runner/test_work_grok.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_executor.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/agent-runner/work_external_review_context.py',
    'deploy/mission-control/agent-runner/work_external_workflow.py',
    'deploy/mission-control/agent-runner/work_identity_contract.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_provider_cooldown.py',
    'deploy/mission-control/agent-runner/work_provider_model_route.py',
    'deploy/mission-control/agent-runner/work_quota_diagnostics.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_worker.py',
    'deploy/mission-control/agent-runner/work_worker_config.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/backend-suites.py',
    'deploy/mission-control/jenkins/README.md',
    'deploy/mission-control/jenkins/setup-ci.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'docs/architecture/external-agent-workflow.md',
    'docs/reports/event-stream-stability-20261125.md',
})
EVENT_STREAM_STABILITY_GUARDS = frozenset({
    'work_workflow.py::WorkflowStore.retry_native_timeout',
    'work_workflow.py::WorkflowStore.retry_peer_verdict',
    'work_workflow.py::WorkflowStore.retry_preparation',
})

# Explicit M40 author-handoff qualification. Every predecessor map below stays
# immutable provenance. Only these declared source paths may use the new bytes;
# M01-M39 migrations and the legacy authority AST comparisons remain raw pins.
# Exact installed M40 functions/owners/ACLs are independently checked by the
# read-only SQL verifier and actual PostgreSQL handoff/schema regressions.
# This source context grants no task approval, CI pass or deployment authority.
CODEX_AUTHOR_HANDOFF_SOURCE_PATHS = frozenset(('AGENT_WORK.md',
 'deploy/mission-control/agent-runner/README.md',
 'deploy/mission-control/agent-runner/quota-handoff-config.example.json',
 'deploy/mission-control/agent-runner/setup-runner.sh',
 'deploy/mission-control/agent-runner/test_agent_handoff.py',
 'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
 'deploy/mission-control/agent-runner/test_author_handoff_schema.py',
 'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
 'deploy/mission-control/agent-runner/test_muse_read_access.py',
 'deploy/mission-control/agent-runner/test_quota_handoff_config.py',
 'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
 'deploy/mission-control/agent-runner/test_work_author_handoff_context.py',
 'deploy/mission-control/agent-runner/test_work_muse_intake.py',
 'deploy/mission-control/agent-runner/test_work_owner_fence.py',
 'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
 'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
 'deploy/mission-control/agent-runner/work_agent_handoff.py',
 'deploy/mission-control/agent-runner/work_author_handoff_context.py',
 'deploy/mission-control/agent-runner/work_ci_contract.py',
 'deploy/mission-control/agent-runner/work_config.py',
 'deploy/mission-control/agent-runner/work_context_recovery.py',
 'deploy/mission-control/agent-runner/work_identity_contract.py',
 'deploy/mission-control/agent-runner/work_peer_context.py',
 'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py',
 'deploy/mission-control/agent-runner/work_provider_model_route.py',
 'deploy/mission-control/agent-runner/work_review_context.py',
 'deploy/mission-control/agent-runner/work_reviewer_correction.py',
 'deploy/mission-control/agent-runner/work_store.py',
 'deploy/mission-control/agent-runner/work_superseding_review.py',
 'deploy/mission-control/agent-runner/work_workflow.py',
 'deploy/mission-control/jenkins/verify-candidate.py',
 'deploy/mission-control/sql/apply.sql',
 'deploy/mission-control/sql/management/verify-agy-unattended.sql',
 'deploy/mission-control/sql/management/verify-author-handoff.sql',
 'deploy/mission-control/sql/management/verify-autonomy.sql',
 'deploy/mission-control/sql/management/verify-feedback.sql',
 'deploy/mission-control/sql/management/verify-retry-events.sql',
 'deploy/mission-control/sql/management/verify.sql',
 'deploy/mission-control/sql/migrations/0040_provider_author_handoff.sql',
 'docs/architecture/agent-quota-handoff.md'))
CODEX_AUTHOR_HANDOFF_SQL_PATHS = frozenset(('deploy/mission-control/sql/apply.sql',
 'deploy/mission-control/sql/management/verify-agy-unattended.sql',
 'deploy/mission-control/sql/management/verify-author-handoff.sql',
 'deploy/mission-control/sql/management/verify-autonomy.sql',
 'deploy/mission-control/sql/management/verify-feedback.sql',
 'deploy/mission-control/sql/management/verify-retry-events.sql',
 'deploy/mission-control/sql/management/verify.sql',
 'deploy/mission-control/sql/migrations/0040_provider_author_handoff.sql'))
CODEX_AUTHOR_HANDOFF_CONTEXT_PATHS = frozenset({
    'AGENT_WORK.md',
    'deploy/mission-control/agent-runner/README.md',
    'deploy/mission-control/agent-runner/quota-handoff-config.example.json',
    'deploy/mission-control/agent-runner/setup-runner.sh',
    'deploy/mission-control/agent-runner/test_agent_handoff.py',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py',
    'deploy/mission-control/agent-runner/test_author_handoff_schema.py',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py',
    'deploy/mission-control/agent-runner/test_muse_read_access.py',
    'deploy/mission-control/agent-runner/test_quota_handoff_config.py',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
    'deploy/mission-control/agent-runner/test_work_author_handoff_context.py',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
    'deploy/mission-control/agent-runner/work_agent_handoff.py',
    'deploy/mission-control/agent-runner/work_author_handoff_context.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_context_recovery.py',
    'deploy/mission-control/agent-runner/work_identity_contract.py',
    'deploy/mission-control/agent-runner/work_peer_context.py',
    'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py',
    'deploy/mission-control/agent-runner/work_provider_model_route.py',
    'deploy/mission-control/agent-runner/work_review_context.py',
    'deploy/mission-control/agent-runner/work_reviewer_correction.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_superseding_review.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/verify-agy-unattended.sql',
    'deploy/mission-control/sql/management/verify-author-handoff.sql',
    'deploy/mission-control/sql/management/verify-autonomy.sql',
    'deploy/mission-control/sql/management/verify-feedback.sql',
    'deploy/mission-control/sql/management/verify-retry-events.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/migrations/0040_provider_author_handoff.sql',
    'docs/architecture/agent-quota-handoff.md',
})
PRE_HANDOFF_MIGRATION_PATHS = frozenset({
    '0001_agent.sql',
    '0002_ide_workspace.sql',
    '0003_ide_chat_history.sql',
    '0004_ide_chat_default_expiration.sql',
    '0005_inference_dispatch.sql',
    '0006_inference_model_selection.sql',
    '0007_ide_mail.sql',
    '0008_mail_archive.sql',
    '0009_coding_admission.sql',
    '0010_coding_execution_and_usage.sql',
    '0011_coding_elevated_tool_policy.sql',
    '0012_coding_progress.sql',
    '0013_coding_task_session_policy.sql',
    '0014_coding_plan_workflow.sql',
    '0015_coding_feedback_loop.sql',
    '0016_coding_autonomous_workflows.sql',
    '0017_coding_codex_warm_sessions.sql',
    '0018_coding_grok_provider.sql',
    '0019_coding_muse_provider.sql',
    '0020_external_agent_transport.sql',
    '0021_coding_event_trace.sql',
    '0022_external_workflows.sql',
    '0023_coding_agy_unattended.sql',
    '0024_native_jenkins_context.sql',
    '0025_native_delivery_repair.sql',
    '0026_coding_owner_continuation.sql',
    '0027_jenkins_events.sql',
    '0028_post_merge_verification.sql',
    '0029_delivery_automation.sql',
    '0030_agent_wide_retry_policy.sql',
    '0031_retry_events.sql',
    '0032_observer_retry_lineage.sql',
    '0033_superseding_reviews.sql',
    '0034_jenkins_stage_started.sql',
    '0035_event_stream_peer_review_diagnostics.sql',
    '0036_event_stream_actor_cause.sql',
    '0037_event_stream_jenkins_producer.sql',
    '0038_external_plan_approve_override.sql',
    '0039_terminal_retry_settlement.sql',
})
HISTORICAL_QUALIFICATION_MAP_NAMES = ('AGENT_WIDE_RETRY_CONTEXT_PATHS',
 'AGENT_WIDE_RETRY_GUARDS',
 'CONSOLIDATED_UI_CONTEXT_PATHS',
 'CONTENT_IDENTITY_CONTEXT_PATHS',
 'DELIVERY_AUTOMATION_CONTEXT_PATHS',
 'EVENT_RETRY_CONTEXT_PATHS',
 'EVENT_RETRY_GUARDS',
 'EVENT_STREAM_STABILITY_CONTEXT_PATHS',
 'EVENT_STREAM_STABILITY_GUARDS',
 'INCLUDED_RETIREMENT_CONTEXT_PATHS',
 'INCLUDED_REVIEWER_CONTEXT_PATHS',
 'JENKINS_PAGED_OBSERVER_CONTEXT_PATHS',
 'LINEAGE_RETRY_CONTEXT_PATHS',
 'M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_PATHS',
 'MIGRATION_PATHS',
 'MUSE_MERGE_COMPATIBILITY_CONTEXT_PATHS',
 'NATIVE_CI_CONTEXT_RECOVERY_PATHS',
 'NATIVE_POSTFIX_CONTEXT_PATHS',
 'PEER_FEEDBACK_CONTEXT_PATHS',
 'PIPELINE_DIAGRAM_CONTEXT_PATHS',
 'PLAN_APPROVE_OVERRIDE_CONTEXT_PATHS',
 'POST_MERGE_CONTEXT_PATHS',
 'REVIEWED_CI_CONTEXT_PATHS',
 'REVIEW_PROTOCOL_CONTEXT_PATHS',
 'SUPERSEDING_REVIEW_CONTEXT_PATHS',
 'TASK_ORGANIZATION_CONTEXT_PATHS',
 'TASK_RECOVERY_CONTEXT_PATHS',
 'TASK_RECOVERY_GUARDS',
 'TERMINAL_PROJECTION_SETTLEMENT_CONTEXT_PATHS',
 'UI_DELIVERY_CONTEXT_PATHS',
 'UI_STAGE_DIAGRAM_CONTEXT_PATHS',
 'UNCHANGED_BASELINE_PATHS',
 'UNCHANGED_LEGACY_GUARDS',
 'VERIFIER_POLICY_PARITY_CONTEXT_PATHS')



STRICT_VERIFIER_FILES = {
    "verify-superseding-reviews.sql",
    "verify-delivery-lineage.sql",
    "verify-retry-events.sql",
    "verify-agent-retries.sql",
    "verify-delivery.sql",
    "verify-post-merge.sql",
    "verify.sql",
    "verify-owner-continuation.sql",
    "verify-agy-unattended.sql",
    "verify-native-jenkins.sql",
    "verify-native-delivery.sql",
    "verify-jenkins-events.sql",
    "verify-cicd-stages.sql",
    "verify-external-agent.sql",
    "verify-muse-read-access.sql",
    "verify-task-events.sql",
    "verify-external-workflows.sql",
    "verify-feedback.sql",
    "verify-autonomy.sql",
    "verify-identity-catalog.sql", "verify-typed-renew.sql", "verify-failure-cause.sql", "verify-deadline-extension-events.sql", "verify-retry-source-key.sql", "verify-gate-contract-names.sql", "verify-delivery-receipt-source-key.sql", "verify-agent-feature-branches.sql",
}


class JenkinsM27SourceBoundaryTest(unittest.TestCase):
    def test_author_handoff_context_is_explicit_and_exact(self):
        self.assertEqual(set(CODEX_AUTHOR_HANDOFF_CONTEXT_PATHS), set(CODEX_AUTHOR_HANDOFF_SOURCE_PATHS))
        self.assertEqual({name for name in CODEX_AUTHOR_HANDOFF_SOURCE_PATHS if '/sql/' in name}, set(CODEX_AUTHOR_HANDOFF_SQL_PATHS))
        self.assertEqual(set(CODEX_AUTHOR_HANDOFF_SQL_PATHS), {
            'deploy/mission-control/sql/apply.sql',
            'deploy/mission-control/sql/management/verify.sql',
            'deploy/mission-control/sql/management/verify-autonomy.sql',
            'deploy/mission-control/sql/management/verify-feedback.sql',
            'deploy/mission-control/sql/management/verify-retry-events.sql',
            'deploy/mission-control/sql/management/verify-agy-unattended.sql',
            'deploy/mission-control/sql/management/verify-author-handoff.sql',
            'deploy/mission-control/sql/migrations/0040_provider_author_handoff.sql',
        })
        for relative in sorted(CODEX_AUTHOR_HANDOFF_CONTEXT_PATHS):
            with self.subTest(author_handoff_source=relative):
                self.assertTrue(relative.startswith('deploy/mission-control/') or relative in {
                    'AGENT_WORK.md', 'docs/architecture/agent-quota-handoff.md'})
                if '/migrations/' in relative:
                    self.assertEqual(relative, 'deploy/mission-control/sql/migrations/0040_provider_author_handoff.sql')
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        installed = (MANAGEMENT / 'verify-author-handoff.sql').read_text()
        for signature in ('coding_workflow_author_handoff(uuid)', 'coding_workflow_author_feedback(uuid,text)',
                'coding_workflow_role_granted(uuid,text,text)', 'claim(text,uuid,text[])',
                'read_workflow_feedback(uuid,uuid)', 'read_feedback(uuid,uuid)'):
            self.assertIn(signature, installed)
        published = {tuple(shlex.split(line, comments=True)) for line in (RUNNER / 'setup-runner.sh').read_text().splitlines() if line.lstrip().startswith('publish ')}
        for module in ('work_agent_handoff.py', 'work_author_handoff_context.py'):
            self.assertIn(('publish', '$directory/' + module, '$target/' + module), published)

    def test_author_handoff_preserves_every_historical_qualification_map(self):
        self.assertEqual(len(HISTORICAL_QUALIFICATION_MAP_NAMES), 34)
        self.assertEqual(len(set(HISTORICAL_QUALIFICATION_MAP_NAMES)), 34)
        for name in HISTORICAL_QUALIFICATION_MAP_NAMES:
            with self.subTest(context=name):
                scope = globals()[name]
                self.assertIsInstance(scope, frozenset)
                self.assertTrue(scope)
                self.assertTrue(all(type(entry) is str and entry for entry in scope))

    def test_author_handoff_cannot_qualify_any_predecessor_migration(self):
        present = {path.name for path in MIGRATIONS.glob('[0-9][0-9][0-9][0-9]_*.sql') if 1 <= int(path.name[:4]) <= 39}
        self.assertEqual(len(PRE_HANDOFF_MIGRATION_PATHS), 39)
        self.assertEqual(present, set(PRE_HANDOFF_MIGRATION_PATHS))
        for name in sorted(PRE_HANDOFF_MIGRATION_PATHS):
            relative = 'deploy/mission-control/sql/migrations/' + name
            with self.subTest(pre_handoff_migration=name):
                self.assertTrue((MIGRATIONS / name).is_file(), name)
                self.assertNotIn(relative, CODEX_AUTHOR_HANDOFF_SOURCE_PATHS)
                self.assertNotIn(relative, CODEX_AUTHOR_HANDOFF_SQL_PATHS)
        for relative in ('deploy/mission-control/sql/management/grant-runner.sql',
                'deploy/mission-control/sql/management/verify-owner-continuation.sql',
                'deploy/mission-control/sql/management/verify-task-events.sql'):
            self.assertNotIn(relative, CODEX_AUTHOR_HANDOFF_SQL_PATHS)

    def test_applied_migrations_one_through_twenty_six_are_present(self):
        historical_files = {
            path.name for path in MIGRATIONS.glob("[0-9][0-9][0-9][0-9]_*.sql")
            if 1 <= int(path.name[:4]) <= 26
        }
        self.assertEqual(historical_files, set(MIGRATION_PATHS))
        for name in sorted(MIGRATION_PATHS):
            with self.subTest(migration=name):
                self.assertTrue((MIGRATIONS / name).is_file(), name)

    def test_unchanged_baseline_boundaries_remain_present(self):
        # Reviewed contexts are subsets of the baseline scope; every file stays
        # present while later work changes its contents.
        self.assertTrue(set(REVIEWED_CI_CONTEXT_PATHS) <= set(UNCHANGED_BASELINE_PATHS))
        current = UNCHANGED_BASELINE_PATHS | REVIEWED_CI_CONTEXT_PATHS | POST_MERGE_CONTEXT_PATHS | UI_DELIVERY_CONTEXT_PATHS
        for relative in sorted(current):
            with self.subTest(source=relative):
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_explicit_reviewer_context_matches_included_unapproved_source(self):
        for name in sorted(INCLUDED_REVIEWER_CONTEXT_PATHS):
            with self.subTest(context=REVIEWER_CONTEXT_REVISION, source=name):
                self.assertTrue((RUNNER / name).is_file(), name)

    def test_task_organization_changes_match_explicit_unapproved_context(self):
        for relative in sorted(TASK_ORGANIZATION_CONTEXT_PATHS):
            with self.subTest(context='unapproved task organization', source=relative):
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_retirement_and_ui_composition_match_their_explicit_source_contexts(self):
        for context, pins in ((RETIREMENT_CONTEXT_REVISION, INCLUDED_RETIREMENT_CONTEXT_PATHS),
                              ('consolidated task organization and retirement', CONSOLIDATED_UI_CONTEXT_PATHS)):
            for relative in sorted(pins):
                with self.subTest(context=context, source=relative):
                    self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_delivery_ui_overlay_is_scoped_to_reviewed_abi14_source(self):
        self.assertEqual(UI_DELIVERY_CONTEXT_REVISION, "f246b1494f29cf86af8a32a24da80d56c17427d1")
        self.assertEqual(len(UI_DELIVERY_CONTEXT_PATHS), 33)
        for relative in sorted(UI_DELIVERY_CONTEXT_PATHS):
            with self.subTest(delivery_ui=relative):
                self.assertTrue(relative.startswith(("src/rust/forge-ide-ui/", "src/rust/forge-ide-web/", "src/rust/forge-ide-web-host/"))
                    or relative in {".github/workflows/ide-ui-browser.yml", "docs/architecture/forge-post-merge-verification.md", "docs/design/implementation-status.md"})
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_delivery_automation_overlay_is_scoped_and_exact(self):
        # The pipeline diagram overlay later changed some of these files; the
        # scope, not the bytes, is the delivery boundary.
        for relative in sorted(DELIVERY_AUTOMATION_CONTEXT_PATHS):
            with self.subTest(delivery_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/', 'src/rust/forge-ide-web-host/')) or relative in {
                    'docs/architecture/forge-post-merge-verification.md', 'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0029_delivery_automation.sql'))
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_shared_retry_overlay_preserves_historical_authority_boundaries(self):
        self.assertEqual(set(AGENT_WIDE_RETRY_GUARDS), {
            'work_owner_control.py::NativeOwnerControl.review_decide',
            'work_workflow.py::WorkflowStore.retry_peer_verdict',
            'work_workflow.py::WorkflowStore.retry_preparation',
        })
        for relative in sorted(AGENT_WIDE_RETRY_CONTEXT_PATHS):
            with self.subTest(shared_retry_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/'))
                    or relative in {'docs/architecture/forge-post-merge-verification.md', 'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0030_agent_wide_retry_policy.sql'))
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_event_retry_overlay_is_explicit_and_preserves_applied_migrations(self):
        self.assertEqual(set(EVENT_RETRY_GUARDS), {
            'work_workflow.py::WorkflowStore.retry_peer_verdict'})
        for relative in sorted(EVENT_RETRY_CONTEXT_PATHS):
            with self.subTest(event_retry_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/'))
                    or relative in {'docs/architecture/forge-post-merge-verification.md',
                        'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0031_retry_events.sql'))
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_lineage_overlay_preserves_applied_migrations_and_exact_source(self):
        for relative in sorted(LINEAGE_RETRY_CONTEXT_PATHS):
            with self.subTest(lineage_source=relative):
                self.assertTrue(relative.startswith('deploy/mission-control/') or relative in {
                    'AGENT_WORK.md', 'docs/architecture/ci-stage-evidence.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0032_observer_retry_lineage.sql'))
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        for relative in sorted(EVENT_RETRY_CONTEXT_PATHS):
            if '/migrations/0031_' in relative or '/fragments/0031_' in relative:
                with self.subTest(applied_m31=relative):
                    self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_superseding_review_overlay_preserves_applied_schema_and_exact_source(self):
        self.assertIn('deploy/mission-control/agent-runner/work_superseding_review.py', SUPERSEDING_REVIEW_CONTEXT_PATHS)
        for relative in sorted(SUPERSEDING_REVIEW_CONTEXT_PATHS):
            with self.subTest(superseding_review_source=relative):
                self.assertTrue(relative.startswith('deploy/mission-control/') or relative in {
                    'AGENT_WORK.md', 'docs/architecture/proposals/forge-autonomous-agent-workflow.md'})
                if '/migrations/' in relative or '/fragments/' in relative:
                    self.assertTrue(relative.endswith('0033_superseding_reviews.sql'))
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        # The new context cannot qualify edits to any previously applied SQL.
        for pins in (EVENT_RETRY_CONTEXT_PATHS, LINEAGE_RETRY_CONTEXT_PATHS):
            for relative in sorted(pins):
                if '/migrations/' in relative or '/fragments/' in relative:
                    with self.subTest(immutable_pre_m33=relative):
                        self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_peer_feedback_overlay_is_exact_and_cannot_qualify_applied_sql(self):
        self.assertEqual(set(PEER_FEEDBACK_CONTEXT_PATHS), {'docs/architecture/proposals/forge-autonomous-agent-workflow.md', 'deploy/mission-control/jenkins/verify-candidate.py', 'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py', 'AGENT_WORK.md', 'deploy/mission-control/agent-runner/work_workflow.py', 'deploy/mission-control/agent-runner/test_work_peer_context.py', 'deploy/mission-control/agent-runner/test_work_peer_feedback_recovery.py', 'deploy/mission-control/agent-runner/work_peer_context.py', 'deploy/mission-control/agent-runner/test_ci_event_phases.py', 'deploy/mission-control/agent-runner/work_ci_contract.py', 'deploy/mission-control/agent-runner/work_context_recovery.py', 'deploy/mission-control/agent-runner/setup-runner.sh', 'deploy/mission-control/agent-runner/test_work_linked_child_feedback.py'})
        for relative in sorted(PEER_FEEDBACK_CONTEXT_PATHS):
            with self.subTest(peer_feedback_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        for pins in (EVENT_RETRY_CONTEXT_PATHS, LINEAGE_RETRY_CONTEXT_PATHS, SUPERSEDING_REVIEW_CONTEXT_PATHS):
            for relative in sorted(pins):
                if '/sql/' in relative:
                    with self.subTest(immutable_applied_sql=relative):
                        self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_review_protocol_overlay_is_exact_and_preserves_applied_authority(self):
        self.assertEqual(set(REVIEW_PROTOCOL_CONTEXT_PATHS), {'deploy/mission-control/agent-runner/work_external_recovery.py', 'deploy/mission-control/agent-runner/work_executor.py', 'deploy/mission-control/agent-runner/work_superseding_review.py', 'deploy/mission-control/agent-runner/test_external_transport_recovery.py', 'deploy/mission-control/agent-runner/work_external_workflow.py', 'deploy/mission-control/agent-runner/test_ci_event_phases.py', 'docs/architecture/external-agent-workflow.md', 'AGENT_WORK.md', 'deploy/mission-control/jenkins/verify-candidate.py', 'deploy/mission-control/agent-runner/test_work_cli_transport.py', 'deploy/mission-control/agent-runner/test_work_review_revision_attestations.py', 'deploy/mission-control/agent-runner/setup-runner.sh', 'deploy/mission-control/agent-runner/work_worker.py', 'deploy/mission-control/agent-runner/README.md', 'deploy/mission-control/agent-runner/work_ci_contract.py', 'deploy/mission-control/agent-runner/work_cli_transport.py'})
        for relative in sorted(REVIEW_PROTOCOL_CONTEXT_PATHS):
            with self.subTest(review_protocol_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        for pins in (EVENT_RETRY_CONTEXT_PATHS, LINEAGE_RETRY_CONTEXT_PATHS, SUPERSEDING_REVIEW_CONTEXT_PATHS):
            for relative in sorted(pins):
                if '/migrations/' in relative or '/fragments/' in relative:
                    with self.subTest(applied_schema=relative):
                        self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_stage_diagram_ui_overlay_is_scoped_to_abi15_ui_source(self):
        self.assertEqual(len(UI_STAGE_DIAGRAM_CONTEXT_PATHS), 23)
        for relative in sorted(UI_STAGE_DIAGRAM_CONTEXT_PATHS):
            with self.subTest(stage_diagram_ui=relative):
                self.assertTrue(
                    relative.startswith(('src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/', 'src/rust/forge-ide-web-host/'))
                    or relative == 'docs/design/implementation-status.md'
                )
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_pipeline_diagram_overlay_is_scoped_and_exact(self):
        self.assertEqual(len(PIPELINE_DIAGRAM_CONTEXT_PATHS), 52)
        for relative in sorted(PIPELINE_DIAGRAM_CONTEXT_PATHS):
            with self.subTest(pipeline_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/', 'src/rust/forge-ide-web-host/')) or relative in {
                    'docs/architecture/forge-post-merge-verification.md', 'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0034_jenkins_stage_started.sql'))
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_muse_merge_compatibility_overlay_is_scoped_and_exact(self):
        self.assertEqual(set(MUSE_MERGE_COMPATIBILITY_CONTEXT_PATHS), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
            'deploy/mission-control/agent-runner/test_work_peer_diagnostics_schema.py',
            'deploy/mission-control/agent-runner/test_work_peer_replay.py',
            'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
            'deploy/mission-control/agent-runner/work_workflow.py',
            'deploy/mission-control/sql/management/verify-autonomy.sql',
            'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
            'deploy/mission-control/sql/migrations/0035_event_stream_peer_review_diagnostics.sql',
        })
        for relative in sorted(MUSE_MERGE_COMPATIBILITY_CONTEXT_PATHS):
            with self.subTest(muse_compatibility_source=relative):
                if '/migrations/' in relative or '/fragments/' in relative:
                    self.assertEqual(relative, 'deploy/mission-control/sql/migrations/0035_event_stream_peer_review_diagnostics.sql')
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_plan_approve_override_overlay_is_scoped_and_preserves_applied_migrations(self):
        self.assertIn('deploy/mission-control/agent-runner/work_external_plan_override.py', PLAN_APPROVE_OVERRIDE_CONTEXT_PATHS)
        self.assertIn('deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql', PLAN_APPROVE_OVERRIDE_CONTEXT_PATHS)
        for relative in sorted(PLAN_APPROVE_OVERRIDE_CONTEXT_PATHS):
            with self.subTest(plan_override_source=relative):
                self.assertTrue(
                    relative.startswith('deploy/mission-control/')
                    or relative in {
                        'docs/architecture/external-agent-workflow.md',
                        'docs/architecture/proposals/external-plan-approve-override.md',
                    }
                )
                if '/migrations/' in relative or '/fragments/' in relative:
                    self.assertEqual(relative, 'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql')
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        published = {
            tuple(shlex.split(line, comments=True))
            for line in (RUNNER / 'setup-runner.sh').read_text().splitlines()
            if line.lstrip().startswith('publish ')
        }
        self.assertIn(('publish', '$directory/work_external_plan_override.py', '$target/work_external_plan_override.py'), published)

    def test_task_recovery_overlay_is_exact_and_keeps_other_guards(self):
        self.assertEqual(set(TASK_RECOVERY_CONTEXT_PATHS), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/test_external_code_context_recovery.py',
            'deploy/mission-control/agent-runner/test_external_diff_contract.py',
            'deploy/mission-control/agent-runner/test_native_deadline_events.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_external_recovery.py',
            'deploy/mission-control/agent-runner/work_external_review_context.py',
            'deploy/mission-control/agent-runner/work_external_workflow.py',
            'deploy/mission-control/agent-runner/work_workflow.py',
            'src/rust/forge-ide-ui/src/model/agents/tests/scopes.rs',
            'src/rust/forge-ide-ui/src/model/agents/workflow.rs',
            'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
            'src/rust/forge-ide-web/tests/browser/journey.mjs',
        })
        self.assertEqual(set(TASK_RECOVERY_GUARDS), {
            'work_workflow.py::WorkflowStore.retry_native_timeout'})
        for relative in sorted(TASK_RECOVERY_CONTEXT_PATHS):
            with self.subTest(task_recovery_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_m36_legacy_progress_verifier_context_is_exact(self):
        self.assertEqual(set(M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_PATHS), {
            'deploy/mission-control/sql/management/verify.sql',
            'deploy/mission-control/sql/management/verify-task-events.sql',
        })
        for relative in sorted(M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_PATHS):
            with self.subTest(accepted_m36_verifier=relative):
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_jenkins_paged_observer_context_is_scoped_and_exact(self):
        self.assertEqual(set(JENKINS_PAGED_OBSERVER_CONTEXT_PATHS), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_jenkins_read_observer.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_jenkins_observer.py',
            'deploy/mission-control/agent-runner/work_publication.py',
            'src/rust/forge-jenkins/README.md',
            'src/rust/forge-jenkins/src/client.rs',
            'src/rust/forge-jenkins/src/lib.rs',
            'src/rust/forge-jenkins/src/observation.rs',
            'src/rust/forge-jenkins/tests/client.rs',
            'src/rust/forge-local/README.md',
            'src/rust/forge-local/src/application.rs',
            'src/rust/forge-local/src/jenkins.rs',
            'src/rust/forge-local/tests/application.rs',
            'src/rust/forge-local/tests/jenkins_application.rs',
            'src/rust/forge-terminal/README.md',
            'src/rust/forge-terminal/src/jenkins.rs',
            'src/rust/forge-terminal/src/parser.rs',
            'src/rust/forge-terminal/tests/jenkins.rs',
        })
        for relative in sorted(JENKINS_PAGED_OBSERVER_CONTEXT_PATHS):
            with self.subTest(jenkins_paged_observer_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_native_ci_context_recovery_is_scoped_and_exact(self):
        self.assertEqual(set(NATIVE_CI_CONTEXT_RECOVERY_PATHS), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_native_ci_reprepare.py',
            'deploy/mission-control/agent-runner/work_native_ci_reprepare.py',
        })
        for relative in sorted(NATIVE_CI_CONTEXT_RECOVERY_PATHS):
            with self.subTest(native_ci_context_source=relative):
                self.assertNotIn("/sql/", relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_content_identity_context_is_scoped_and_exact(self):
        self.assertEqual(set(CONTENT_IDENTITY_CONTEXT_PATHS), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_external_content_identity.py',
            'deploy/mission-control/agent-runner/test_native_content_identity.py',
            'deploy/mission-control/agent-runner/test_work_artifact_identity.py',
            'deploy/mission-control/agent-runner/test_work_identity_store.py',
            'deploy/mission-control/agent-runner/work_artifact_identity.py',
            'deploy/mission-control/agent-runner/work_artifact_source.py',
            'deploy/mission-control/agent-runner/work_config.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_external_identity.py',
            'deploy/mission-control/agent-runner/work_external_review_context.py',
            'deploy/mission-control/agent-runner/work_external_workflow.py',
            'deploy/mission-control/agent-runner/work_identity_contract.py',
            'deploy/mission-control/agent-runner/work_identity_store.py',
            'deploy/mission-control/agent-runner/work_native_identity.py',
            'deploy/mission-control/agent-runner/work_native_jenkins.py',
            'deploy/mission-control/agent-runner/work_publication.py',
            'deploy/mission-control/agent-runner/work_publication_config.py',
            'deploy/mission-control/agent-runner/work_review.py',
            'deploy/mission-control/agent-runner/work_worker.py',
            'docs/README.md',
            'docs/architecture/README.md',
            'docs/architecture/content-identity.md',
            'docs/architecture/diagrams/mermaid/README.md',
            'docs/architecture/diagrams/mermaid/content-identity-components.mmd',
            'docs/architecture/diagrams/mermaid/content-identity-data.mmd',
            'docs/architecture/diagrams/mermaid/content-identity-external.mmd',
            'docs/architecture/diagrams/mermaid/content-identity-native.mmd',
            'docs/architecture/diagrams/mermaid/images/content-identity-components.svg',
            'docs/architecture/diagrams/mermaid/images/content-identity-data.svg',
            'docs/architecture/diagrams/mermaid/images/content-identity-external.svg',
            'docs/architecture/diagrams/mermaid/images/content-identity-native.svg',
            'docs/architecture/diagrams/mermaid/render-manifest.json',
            'docs/architecture/diagrams/mermaid/render.py',
            'docs/architecture/diagrams/mermaid/validate.mjs',
            'docs/design/implementation-status.md',
            'src/rust/forge-ide-web-host/build.rs',
            'src/rust/forge-ide-web-host/src/assets.rs',
            'src/rust/forge-ide-web-host/src/design/content_identity.rs',
            'src/rust/forge-ide-web-host/src/design/landing.rs',
            'src/rust/forge-ide-web-host/src/design/mod.rs',
            'src/rust/forge-ide-web-host/tests/consumer.rs',
        })
        for relative in sorted(CONTENT_IDENTITY_CONTEXT_PATHS):
            with self.subTest(content_identity_source=relative):
                self.assertNotIn("/sql/", relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        published={tuple(shlex.split(line,comments=True)) for line in (RUNNER / "setup-runner.sh").read_text().splitlines() if line.lstrip().startswith("publish ")}
        for name in ("work_artifact_identity.py","work_artifact_source.py","work_identity_contract.py","work_identity_store.py","work_native_identity.py","work_external_identity.py"):
            self.assertIn(("publish","$directory/"+name,"$target/"+name),published)

    def test_native_postfix_context_is_scoped_and_exact(self):
        self.assertEqual(set(NATIVE_POSTFIX_CONTEXT_PATHS), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_native_ci_postfix.py',
            'deploy/mission-control/agent-runner/test_native_ci_postfix_proof.py',
            'deploy/mission-control/agent-runner/work_identity_contract.py',
            'deploy/mission-control/agent-runner/work_native_ci_postfix.py',
            'deploy/mission-control/agent-runner/work_native_ci_postfix_proof.py',
        })
        for relative in sorted(NATIVE_POSTFIX_CONTEXT_PATHS):
            with self.subTest(native_postfix_source=relative):
                self.assertNotIn("/sql/", relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_verifier_policy_parity_overlay_is_scoped_and_preserves_applied_sql(self):
        self.assertEqual(set(VERIFIER_POLICY_PARITY_CONTEXT_PATHS), {
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_verifier_policy_parity.py',
            'deploy/mission-control/agent-runner/test_work_artifact_identity.py',
            'deploy/mission-control/agent-runner/work_ci_contract.py',
            'deploy/mission-control/agent-runner/work_ci_phases.py',
            'deploy/mission-control/agent-runner/work_delivery_repair.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_native_jenkins.py',
            'deploy/mission-control/jenkins/verify-candidate.py',
        })
        for relative in sorted(VERIFIER_POLICY_PARITY_CONTEXT_PATHS):
            with self.subTest(verifier_policy_parity_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_terminal_projection_settlement_overlay_is_scoped_and_preserves_applied_migrations(self):
        self.assertIn('deploy/mission-control/sql/migrations/0039_terminal_retry_settlement.sql', TERMINAL_PROJECTION_SETTLEMENT_CONTEXT_PATHS)
        self.assertIn('deploy/mission-control/sql/management/repair-stale-terminal-projections.sql', TERMINAL_PROJECTION_SETTLEMENT_CONTEXT_PATHS)
        self.assertIn('deploy/mission-control/agent-runner/test_terminal_projection_settlement.py', TERMINAL_PROJECTION_SETTLEMENT_CONTEXT_PATHS)
        for relative in sorted(TERMINAL_PROJECTION_SETTLEMENT_CONTEXT_PATHS):
            with self.subTest(terminal_projection_source=relative):
                self.assertTrue(
                    relative.startswith('deploy/mission-control/')
                    or relative == 'src/rust/forge-ide-ui/src/model/agents/tests/workflow.rs'
                )
                self.assertNotIn('/fragments/', relative)
                if '/migrations/' in relative:
                    self.assertEqual(relative, 'deploy/mission-control/sql/migrations/0039_terminal_retry_settlement.sql')
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)

    def test_event_stream_stability_context_is_scoped_and_exact(self):
        self.assertEqual(set(EVENT_STREAM_STABILITY_CONTEXT_PATHS), {'docs/reports/event-stream-stability-20261125.md', 'deploy/mission-control/agent-runner/test_native_ci_reconcile_stability.py', 'deploy/mission-control/agent-runner/work_external_delivery.py', 'deploy/mission-control/agent-runner/test_agents_runtime_integration.py', 'deploy/mission-control/agent-runner/work_worker_config.py', 'AGENTS.md', 'deploy/mission-control/agent-runner/work_config.py', 'deploy/mission-control/agent-runner/test_provider_model_route.py', 'deploy/mission-control/agent-runner/test_external_content_identity.py', 'deploy/mission-control/agent-runner/test_ci_restart_deferral.py', 'deploy/mission-control/agent-runner/test_stopped_binary_pin.py', 'deploy/mission-control/agent-runner/test_provider_quota_audit.py', 'AGENT_WORK.md', 'deploy/mission-control/agent-runner/test_external_main_refresh.py', 'deploy/mission-control/agent-runner/test_grok_trust.py', 'deploy/mission-control/agent-runner/test_external_flow.py', 'deploy/mission-control/agent-runner/work_native_jenkins.py', 'docs/architecture/external-agent-workflow.md', 'deploy/mission-control/agent-runner/work_external_workflow.py', 'deploy/mission-control/agent-runner/test_backend_main_parallel.py', 'deploy/mission-control/agent-runner/work_external_review_context.py', 'deploy/mission-control/agent-runner/work_provider_model_route.py', 'deploy/mission-control/agent-runner/work_worker.py', 'deploy/mission-control/agent-runner/work_identity_contract.py', 'deploy/mission-control/agent-runner/work_workflow.py', 'deploy/mission-control/agent-runner/test_provider_authority_ci_policy.py', 'deploy/mission-control/jenkins/verify-candidate.py', 'deploy/mission-control/agent-runner/test_external_artifact_repair.py', 'deploy/mission-control/agent-runner/work_provider_cooldown.py', 'deploy/mission-control/jenkins/README.md', 'deploy/mission-control/agent-runner/test_provider_quota_diagnostics.py', 'deploy/mission-control/agent-runner/setup-runner.sh', 'deploy/mission-control/agent-runner/work_quota_diagnostics.py', 'deploy/mission-control/agent-runner/test_grok_auth_cleanup.py', 'deploy/mission-control/agent-runner/work_store.py', 'deploy/mission-control/agent-runner/work_executor.py', 'deploy/mission-control/agent-runner/test_provider_cooldown.py', 'deploy/mission-control/agent-runner/test_muse_auth_cleanup.py', 'deploy/mission-control/agent-runner/test_work_grok.py', 'deploy/mission-control/agent-runner/work_ci_contract.py', 'deploy/mission-control/backend-suites.py', 'deploy/mission-control/jenkins/setup-ci.py'})
        self.assertEqual(set(EVENT_STREAM_STABILITY_GUARDS), {'work_workflow.py::WorkflowStore.retry_native_timeout', 'work_workflow.py::WorkflowStore.retry_peer_verdict', 'work_workflow.py::WorkflowStore.retry_preparation'})
        for relative in sorted(EVENT_STREAM_STABILITY_CONTEXT_PATHS):
            with self.subTest(event_stream_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertTrue((REPO_ROOT / relative).is_file(), relative)
        source = (RUNNER / 'setup-runner.sh').read_text()
        for name in ('work_quota_diagnostics.py', 'work_provider_model_route.py', 'work_provider_cooldown.py'):
            self.assertIn('publish "$directory/' + name + '" "$target/' + name + '"', source)

    def test_retired_browser_reducers_and_dispatchers_are_absent(self):
        for relative in RETIRED_BROWSER_SOURCE_FILES:
            with self.subTest(retired=relative):
                self.assertFalse((REPO_ROOT / relative).exists())

    def test_legacy_owner_and_publication_guards_remain_defined(self):
        # Their behavior is exercised by the functional and PostgreSQL
        # regressions; this keeps every named authority guard in place.
        overlays = AGENT_WIDE_RETRY_GUARDS | EVENT_RETRY_GUARDS | TASK_RECOVERY_GUARDS | EVENT_STREAM_STABILITY_GUARDS
        self.assertTrue(overlays <= UNCHANGED_LEGACY_GUARDS)
        for identity in sorted(UNCHANGED_LEGACY_GUARDS):
            filename, qualified = identity.split('::')
            with self.subTest(guard=identity):
                node = ast.parse((RUNNER / filename).read_text())
                for name in qualified.split('.'):
                    node = next((child for child in node.body if getattr(child, 'name', None) == name), None)
                    self.assertIsNotNone(node, identity)
                self.assertIsInstance(node, (ast.FunctionDef, ast.ClassDef))
                if isinstance(node, ast.FunctionDef):
                    self.assertTrue(any(isinstance(child, (ast.Raise, ast.Return, ast.If))
                                        for child in ast.walk(node)), identity)

    def test_applied_migrations_and_fragments_are_append_only(self):
        """Applied schema files never change; new schema arrives as a new number."""
        names = sorted(path.name for path in MIGRATIONS.glob('[0-9][0-9][0-9][0-9]_*.sql'))
        numbers = [int(name[:4]) for name in names]
        self.assertEqual(numbers, list(range(1, len(numbers) + 1)))
        self.assertGreaterEqual(len(numbers), APPLIED_SCHEMA_VERSION)
        root = subprocess.run(['git', '-C', str(REPO_ROOT), 'rev-parse', '--show-toplevel'],
                              capture_output=True, text=True)
        known = subprocess.run(['git', '-C', str(REPO_ROOT), 'cat-file', '-e',
                                APPLIED_SCHEMA_REVISION + '^{commit}'], capture_output=True)
        if root.returncode or Path(root.stdout.strip()).resolve() != REPO_ROOT or known.returncode:
            self.skipTest('Git history with the applied-schema revision is unavailable')
        applied = [f'deploy/mission-control/sql/migrations/{name}' for name in names
                   if int(name[:4]) <= APPLIED_SCHEMA_VERSION]
        applied += [f'deploy/mission-control/sql/fragments/{path.name}'
                    for path in sorted((MIGRATIONS.parent / 'fragments').glob('[0-9][0-9][0-9][0-9]_*.sql'))
                    if int(path.name[:4]) <= APPLIED_SCHEMA_VERSION]
        changed = subprocess.run(['git', '-C', str(REPO_ROOT), 'diff', '--name-only',
                                  APPLIED_SCHEMA_REVISION, '--', *applied],
                                 capture_output=True, text=True, check=True).stdout.split()
        self.assertEqual(changed, [], 'applied migrations are immutable; add a new migration instead')

    def test_strict_verifier_closure_retains_owner_and_adds_jenkins(self):
        pending = [MANAGEMENT / "verify.sql"]
        visited = set()
        while pending:
            source = pending.pop().resolve()
            if source in visited:
                continue
            self.assertTrue(source.is_relative_to(MANAGEMENT.resolve()), source)
            self.assertTrue(source.is_file(), source)
            visited.add(source)
            for name in re.findall(r"^\\ir\s+(\S+)\s*$", source.read_text(), re.MULTILINE):
                pending.append(source.parent / name)
        self.assertEqual({source.name for source in visited}, STRICT_VERIFIER_FILES | {"verify-author-handoff.sql", "verify-continuation-plan-lineage.sql"})

    def test_normal_installer_preserves_owner_control_and_installs_event_service(self):
        published = {
            tuple(shlex.split(line, comments=True))
            for line in (RUNNER / "setup-runner.sh").read_text().splitlines()
            if line.lstrip().startswith("publish ")
        }
        required = (
            "work_owner_control.py", "work_owner_reverification.py", "work_publication_clients.py", "work_review.py",
            "work_store.py", "work_worker_store.py", "work_reviewer_correction.py", "work_superseding_review.py",
            "work_peer_feedback_recovery.py", "work_cli_transport.py",
            "work_post_merge.py", "work_post_merge_provider.py", "jenkins_events.py", "jenkins_config.py",
            "jenkins_codec.py", "jenkins_store.py", "jenkins_consumer.py",
            "jenkins_api.py", "jenkins_emitter.py", "jenkins_emit_tool.py",
            "work_jenkins_lifecycle.py", "work_pipeline_projection.py",
        )
        for name in required:
            with self.subTest(module=name):
                self.assertTrue((RUNNER / name).is_file())
                self.assertIn(("publish", "$directory/" + name, "$target/" + name), published)


if __name__ == "__main__":
    unittest.main()
