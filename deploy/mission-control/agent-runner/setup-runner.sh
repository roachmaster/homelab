#!/usr/bin/env bash
set -euo pipefail

[[ "$(hostname -s)" == mission-control && "$EUID" == 0 ]] ||
    { printf '%s\n' 'Run this on Mission Control with sudo.' >&2; exit 1; }
[[ "$#" -eq 0 ]] || { printf 'Usage: %s\n' "$0" >&2; exit 1; }

directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
target=/opt/homelab-forge/agent-runner
secrets_directory=/etc/homelab-forge/agent-runner
unit=/etc/systemd/system/forge-agent-runner.service

"$directory/../setup-postgres.sh" --check
/usr/bin/python3 -B - "$directory" <<'PY'
import json
from pathlib import Path
import sys

sys.path.insert(0, sys.argv[1])
from work_ci_contract import verify_verifier_policy_parity

pairs = (
    (Path('/etc/forge-ci/native-pipeline.json'), Path('/usr/local/libexec/forge-native-ci-check')),
    (Path('/etc/forge-ci/pipeline.json'), Path('/usr/local/libexec/forge-ci-check')),
)
for config_path, verifier_path in pairs:
    if not config_path.exists() and not verifier_path.exists():
        continue
    if not config_path.is_file() or not verifier_path.is_file():
        sys.exit(f'Incomplete installed Forge CI verifier pair: {config_path} / {verifier_path}')
    pipeline = json.loads(config_path.read_text(encoding='utf-8'))
    try:
        verify_verifier_policy_parity(verifier_path.read_bytes(), pipeline.get('verification_profile'))
    except ValueError as error:
        sys.exit(f'{verifier_path}: {error}')
PY
test -s /etc/homelab-forge/web/llama.token
if ! /usr/bin/python3 -c 'import psycopg2'; then
    DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l \
        apt-get install -y --no-install-recommends python3-psycopg2
fi

install -d -o root -g root -m 0755 "$target"
install -d -o root -g root -m 0700 "$secrets_directory"
if [[ ! -s "$secrets_directory/token" ]]; then
    temporary="$(mktemp "$secrets_directory/.token-XXXXXXXX")"
    trap 'rm -f -- "$temporary"' EXIT
    /usr/bin/python3 -c 'import secrets; print(secrets.token_hex(32))' > "$temporary"
    mv -- "$temporary" "$secrets_directory/token"
    trap - EXIT
fi

changed=false
publish() {
    if ! cmp -s "$1" "$2"; then
        install -o root -g root -m 0644 "$1" "$2"
        changed=true
    fi
}
publish "$directory/queue_store.py" "$target/queue_store.py"
publish "$directory/chat_history_store.py" "$target/chat_history_store.py"
publish "$directory/chat_history_api.py" "$target/chat_history_api.py"
publish "$directory/chat_run_api.py" "$target/chat_run_api.py"
publish "$directory/mail_store.py" "$target/mail_store.py"
publish "$directory/mail_api.py" "$target/mail_api.py"
publish "$directory/mail_tokenizer.py" "$target/mail_tokenizer.py"
publish "$directory/file_context.py" "$target/file_context.py"
publish "$directory/model_adapter.py" "$target/model_adapter.py"
publish "$directory/agent_tools.py" "$target/agent_tools.py"
publish "$directory/work_policy.py" "$target/work_policy.py"
publish "$directory/work_retry_policy.py" "$target/work_retry_policy.py"
publish "$directory/work_workflow_policy.py" "$target/work_workflow_policy.py"
publish "$directory/work_workflow_continuation.py" "$target/work_workflow_continuation.py"
publish "$directory/work_plan_lineage_recovery.py" "$target/work_plan_lineage_recovery.py"
publish "$directory/work_retry_events.py" "$target/work_retry_events.py"
publish "$directory/work_retry_continuation.py" "$target/work_retry_continuation.py"
publish "$directory/work_quota_diagnostics.py" "$target/work_quota_diagnostics.py"
publish "$directory/work_provider_model_route.py" "$target/work_provider_model_route.py"
publish "$directory/work_provider_cooldown.py" "$target/work_provider_cooldown.py"
publish "$directory/work_agent_handoff.py" "$target/work_agent_handoff.py"
publish "$directory/work_author_handoff_context.py" "$target/work_author_handoff_context.py"
publish "$directory/work_config.py" "$target/work_config.py"
publish "$directory/work_external_policy.py" "$target/work_external_policy.py"
publish "$directory/work_external.py" "$target/work_external.py"
publish "$directory/work_external_flow_config.py" "$target/work_external_flow_config.py"
publish "$directory/work_external_workflow.py" "$target/work_external_workflow.py"
publish "$directory/work_external_recovery.py" "$target/work_external_recovery.py"
publish "$directory/work_external_plan_override.py" "$target/work_external_plan_override.py"
publish "$directory/work_external_review_context.py" "$target/work_external_review_context.py"
publish "$directory/work_external_delivery_store.py" "$target/work_external_delivery_store.py"
publish "$directory/work_external_delivery.py" "$target/work_external_delivery.py"
publish "$directory/work_jenkins_observer.py" "$target/work_jenkins_observer.py"
publish "$directory/work_native_ci_reprepare.py" "$target/work_native_ci_reprepare.py"
publish "$directory/work_native_ci_postfix.py" "$target/work_native_ci_postfix.py"
publish "$directory/work_native_ci_postfix_proof.py" "$target/work_native_ci_postfix_proof.py"
publish "$directory/work_artifact_identity.py" "$target/work_artifact_identity.py"
publish "$directory/work_artifact_source.py" "$target/work_artifact_source.py"
publish "$directory/work_identity_contract.py" "$target/work_identity_contract.py"
publish "$directory/work_identity_store.py" "$target/work_identity_store.py"
publish "$directory/work_native_identity.py" "$target/work_native_identity.py"
publish "$directory/work_external_identity.py" "$target/work_external_identity.py"
publish "$directory/work_store.py" "$target/work_store.py"
publish "$directory/work_events.py" "$target/work_events.py"
publish "$directory/work_failure_cause.py" "$target/work_failure_cause.py"
publish "$directory/work_event_attribution.py" "$target/work_event_attribution.py"
publish "$directory/work_api.py" "$target/work_api.py"
publish "$directory/work_defect.py" "$target/work_defect.py"
publish "$directory/work_owner_control.py" "$target/work_owner_control.py"
publish "$directory/work_owner_reverification.py" "$target/work_owner_reverification.py"
publish "$directory/usage_store.py" "$target/usage_store.py"
publish "$directory/usage_api.py" "$target/usage_api.py"
publish "$directory/usage_types.py" "$target/usage_types.py"
publish "$directory/usage_rpc.py" "$target/usage_rpc.py"
publish "$directory/usage_provider.py" "$target/usage_provider.py"
publish "$directory/usage_muse.py" "$target/usage_muse.py"
publish "$directory/usage_collector.py" "$target/usage_collector.py"
publish "$directory/work_lease_contract.py" "$target/work_lease_contract.py"
publish "$directory/work_worker_store.py" "$target/work_worker_store.py"
publish "$directory/work_worker_config.py" "$target/work_worker_config.py"
publish "$directory/work_executor.py" "$target/work_executor.py"
publish "$directory/work_cli_transport.py" "$target/work_cli_transport.py"
publish "$directory/work_grok.py" "$target/work_grok.py"
publish "$directory/work_muse.py" "$target/work_muse.py"
publish "$directory/work_native_auth.py" "$target/work_native_auth.py"
publish "$directory/work_sessions.py" "$target/work_sessions.py"
publish "$directory/work_process.py" "$target/work_process.py"
publish "$directory/work_progress.py" "$target/work_progress.py"
publish "$directory/work_worker.py" "$target/work_worker.py"
publish "$directory/work_publication.py" "$target/work_publication.py"
publish "$directory/work_publication_config.py" "$target/work_publication_config.py"
publish "$directory/work_ci_contract.py" "$target/work_ci_contract.py"
publish "$directory/work_ci_phases.py" "$target/work_ci_phases.py"
publish "$directory/work_post_merge.py" "$target/work_post_merge.py"
publish "$directory/work_post_merge_provider.py" "$target/work_post_merge_provider.py"
publish "$directory/work_delivery_ops.py" "$target/work_delivery_ops.py"
publish "$directory/work_delivery_worker.py" "$target/work_delivery_worker.py"
publish "$directory/work_delivery_repair.py" "$target/work_delivery_repair.py"
publish "$directory/work_publication_clients.py" "$target/work_publication_clients.py"
publish "$directory/work_publication_checks.py" "$target/work_publication_checks.py"
publish "$directory/work_feedback.py" "$target/work_feedback.py"
publish "$directory/work_repair_policy.py" "$target/work_repair_policy.py"
publish "$directory/work_review.py" "$target/work_review.py"
publish "$directory/work_review_context.py" "$target/work_review_context.py"
publish "$directory/work_peer_review.py" "$target/work_peer_review.py"
publish "$directory/work_reviewer_correction.py" "$target/work_reviewer_correction.py"
publish "$directory/work_superseding_review.py" "$target/work_superseding_review.py"
publish "$directory/work_peer_context.py" "$target/work_peer_context.py"
publish "$directory/work_context_recovery_contract.py" "$target/work_context_recovery_contract.py"
publish "$directory/work_context_recovery.py" "$target/work_context_recovery.py"
publish "$directory/work_peer_feedback_recovery.py" "$target/work_peer_feedback_recovery.py"
publish "$directory/work_workflow.py" "$target/work_workflow.py"
publish "$directory/work_merge.py" "$target/work_merge.py"
publish "$directory/work_source_revision.py" "$target/work_source_revision.py"
publish "$directory/work_candidate_source.py" "$target/work_candidate_source.py"
publish "$directory/work_consumer_cache.py" "$target/work_consumer_cache.py"
publish "$directory/work_hosted_checks.py" "$target/work_hosted_checks.py"
publish "$directory/work_native_jenkins.py" "$target/work_native_jenkins.py"
publish "$directory/work_native_repair.py" "$target/work_native_repair.py"
publish "$directory/work_build_context.py" "$target/work_build_context.py"
publish "$directory/work_verification.py" "$target/work_verification.py"
publish "$directory/work_verification_init.py" "$target/work_verification_init.py"
publish "$directory/jenkins_events.py" "$target/jenkins_events.py"
publish "$directory/jenkins_config.py" "$target/jenkins_config.py"
publish "$directory/jenkins_codec.py" "$target/jenkins_codec.py"
publish "$directory/jenkins_store.py" "$target/jenkins_store.py"
publish "$directory/jenkins_consumer.py" "$target/jenkins_consumer.py"
publish "$directory/jenkins_api.py" "$target/jenkins_api.py"
publish "$directory/jenkins_emitter.py" "$target/jenkins_emitter.py"
publish "$directory/jenkins_emit_tool.py" "$target/jenkins_emit_tool.py"
publish "$directory/work_jenkins_lifecycle.py" "$target/work_jenkins_lifecycle.py"
publish "$directory/work_pipeline_projection.py" "$target/work_pipeline_projection.py"
publish "$directory/work_agent_branch.py" "$target/work_agent_branch.py"
publish "$directory/work_cicd_artifacts.py" "$target/work_cicd_artifacts.py"
publish "$directory/runner.py" "$target/runner.py"
publish "$directory/forge-agent-runner.service" "$unit"
systemctl daemon-reload
systemctl enable forge-agent-runner.service
if [[ "$changed" == true ]] && systemctl is-active --quiet forge-agent-runner.service; then
    systemctl restart forge-agent-runner.service
fi
systemctl start forge-agent-runner.service

/usr/bin/python3 - "$secrets_directory/token" <<'PY'
from pathlib import Path
import http.client
import sys
import time

token = Path(sys.argv[1]).read_text().strip()
for _ in range(40):
    connection = http.client.HTTPConnection("127.0.0.1", 18083, timeout=1)
    try:
        connection.request("GET", "/health", headers={"Authorization": f"Bearer {token}"})
        response = connection.getresponse()
        response.read()
        if response.status == 200:
            print("Forge agent worker ready")
            break
    except (OSError, http.client.HTTPException):
        pass
    finally:
        connection.close()
    time.sleep(0.25)
else:
    sys.exit("Forge agent worker did not become ready")
PY
