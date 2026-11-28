//! The trusted delivery provider, its factory, and the
//! external delivery worker (the provider half of
//! work_external_delivery.py).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use forge_work::external_review::{ExternalSourceIntegration, EXTERNAL_DIFF_FLAGS};

use crate::artifact_identity::VerificationPolicySnapshot;
use crate::ci_contract::SCOPED_PROFILE;
use crate::ci_phases::EVENT_PROFILE;
use crate::durable_files::{durable_json, effective_uid, group_id, home_dir, read_private};
use crate::external_delivery::{BranchUpsert, ExternalDeliveryLease, ExternalDeliveryStorePort};
use crate::external_identity::{ci_policy_body, ExternalCandidateIdentityProvider};
use crate::identity_contract::content_policy_path;
use crate::identity_store::{
    persist_identity_snapshot_bounded, read_identity_snapshot, read_identity_snapshot_bounded,
};
use crate::publication_clients::{artifact_guard, CommandExecutor};
use crate::trusted_delivery::{
    canonical_branch_slug, canonical_bytes, contract_for_config, gate_ref, jenkins_gate_manifest,
    protected_delivery_gate, protected_delivery_gate_for, sha, validate_branch_name, validate_uid,
    DeliveryConfig, DeliveryError, DeliveryResult, ForgeJenkinsPort, RequireLease,
    SourceArtifactRejected, ARTIFACT_REJECTION_CODES, EPHEMERAL_BRANCH_PREFIX, PARAMETERS,
};
use crate::verifier_parity::PythonAstPort;

fn value_error<T>(message: &str) -> DeliveryResult<T> {
    Err(DeliveryError::value(message.to_string()))
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn identity_store_message(error: crate::identity_store::IdentityStoreError) -> String {
    match error {
        crate::identity_store::IdentityStoreError::Busy => {
            "Identity persistence is busy".to_string()
        }
        crate::identity_store::IdentityStoreError::Invalid(message) => message,
    }
}

fn parse_json(raw: &[u8]) -> DeliveryResult<Value> {
    serde_json::from_slice(raw)
        .map_err(|_| DeliveryError::value("Invalid JSON observation".to_string()))
}

/// The trusted port: owns Git, the Jenkins CLI port, and the
/// (optional) content identity provider. Models never
/// receive these credentials.
pub struct TrustedDeliveryPort {
    pub config: DeliveryConfig,
    pub jenkins: ForgeJenkinsPort,
    pub execute: Box<dyn CommandExecutor>,
    pub identities: Option<ExternalCandidateIdentityProvider>,
    pub env: Vec<(String, String)>,
    pub owner: u32,
}

impl TrustedDeliveryPort {
    pub fn new(
        config: DeliveryConfig,
        jenkins: ForgeJenkinsPort,
        execute: Box<dyn CommandExecutor>,
        identities: Option<ExternalCandidateIdentityProvider>,
    ) -> Self {
        let env = vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("HOME".to_string(), home_dir()),
            (
                "GH_CONFIG_DIR".to_string(),
                config.github_config_dir.to_string_lossy().into_owned(),
            ),
            ("GH_HOST".to_string(), "github.com".to_string()),
            ("GH_PROMPT_DISABLED".to_string(), "1".to_string()),
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
            ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
            ("GIT_NO_REPLACE_OBJECTS".to_string(), "1".to_string()),
            ("GIT_AUTHOR_NAME".to_string(), "Forge publisher".to_string()),
            (
                "GIT_COMMITTER_NAME".to_string(),
                "Forge publisher".to_string(),
            ),
            (
                "GIT_AUTHOR_EMAIL".to_string(),
                "forge@thedevex.com".to_string(),
            ),
            (
                "GIT_COMMITTER_EMAIL".to_string(),
                "forge@thedevex.com".to_string(),
            ),
        ];
        TrustedDeliveryPort {
            config,
            jenkins,
            execute,
            identities,
            env,
            owner: effective_uid(),
        }
    }

    pub fn git(
        &mut self,
        repository: Option<&Path>,
        arguments: &[&str],
        data: Option<&[u8]>,
        env: Option<&[(String, String)]>,
        timeout: f64,
    ) -> DeliveryResult<Vec<u8>> {
        let mut argv = vec!["/usr/bin/git".to_string()];
        if let Some(repository) = repository {
            argv.push("-C".to_string());
            argv.push(repository.to_string_lossy().into_owned());
        }
        argv.extend(arguments.iter().map(|item| item.to_string()));
        let output = self.execute.execute(
            &argv,
            env.unwrap_or(&self.env),
            None,
            data,
            timeout.min(45.0),
        )?;
        Ok(output)
    }

    pub fn git_text(
        &mut self,
        repository: Option<&Path>,
        arguments: &[&str],
        data: Option<&[u8]>,
        env: Option<&[(String, String)]>,
        timeout: f64,
    ) -> DeliveryResult<String> {
        let output = self.git(repository, arguments, data, env, timeout)?;
        Ok(String::from_utf8_lossy(&output).trim().to_string())
    }

    pub fn api(
        &mut self,
        project: &str,
        method: &str,
        suffix: &str,
        payload: Option<&Value>,
    ) -> DeliveryResult<Value> {
        let repository = self
            .config
            .projects
            .get(project)
            .map(|item| item.repository.clone())
            .unwrap_or_default();
        let mut argv = vec![
            "/usr/bin/gh".to_string(),
            "api".to_string(),
            "--method".to_string(),
            method.to_string(),
            format!("repos/{repository}/{suffix}"),
        ];
        let data;
        if let Some(payload) = payload {
            argv.push("--input".to_string());
            argv.push("-".to_string());
            data = canonical_bytes(payload);
        } else {
            data = Vec::new();
        }
        let output = self.execute.execute(
            &argv,
            &self.env,
            None,
            if payload.is_some() { Some(&data) } else { None },
            30.0,
        )?;
        parse_json(&output)
    }

    pub fn source_head(&mut self, project_alias: &str) -> DeliveryResult<String> {
        let Some(project) = self.config.projects.get(project_alias).cloned() else {
            return value_error("Unconfigured source project");
        };
        let result = self.api(project_alias, "GET", "git/ref/heads/main", None)?;
        if result["ref"].as_str() != Some("refs/heads/main")
            || result["object"]["type"].as_str() != Some("commit")
        {
            return value_error("Invalid source ref");
        }
        let observed = sha(result["object"]["sha"].as_str().unwrap_or_default())?;
        let repository = project.canonical_repo.clone();
        let verify = format!("{observed}^{{commit}}");
        let available = self.git_text(
            Some(&repository),
            &["rev-parse", "--verify", &verify],
            None,
            None,
            45.0,
        );
        let available = match available {
            Ok(text) => text,
            Err(DeliveryError::Publication(_)) => {
                let remote = format!("https://github.com/{}.git", project.repository);
                self.git(
                    Some(&repository),
                    &[
                        "-c",
                        "credential.helper=!/usr/bin/gh auth git-credential",
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--",
                        &remote,
                        &observed,
                    ],
                    None,
                    None,
                    45.0,
                )?;
                self.git_text(
                    Some(&repository),
                    &["rev-parse", "--verify", &verify],
                    None,
                    None,
                    45.0,
                )?
            }
            Err(error) => return Err(error),
        };
        if available != observed {
            return value_error("Configured source object identity changed");
        }
        Ok(observed)
    }

    pub fn directory(&self, request: &Value) -> DeliveryResult<PathBuf> {
        validate_uid(request["action_id"].as_str().unwrap_or_default())?;
        validate_uid(request["workflow_id"].as_str().unwrap_or_default())?;
        if !self
            .config
            .projects
            .contains_key(request["project_alias"].as_str().unwrap_or_default())
        {
            return value_error("Delivery project denied");
        }
        let path = self
            .config
            .state_root
            .join("actions")
            .join(request["action_id"].as_str().unwrap_or_default());
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)
                .map_err(|error| DeliveryError::value(error.to_string()))?;
        }
        if path
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return value_error("Unsafe delivery state");
        }
        Ok(path)
    }

    fn remote_branch_heads(
        &mut self,
        repository: &Path,
        remote: &str,
    ) -> DeliveryResult<BTreeMap<String, String>> {
        let output = self.git(
            Some(repository),
            &[
                "-c",
                "credential.helper=!/usr/bin/gh auth git-credential",
                "ls-remote",
                "--heads",
                "--",
                remote,
            ],
            None,
            None,
            45.0,
        )?;
        let text = String::from_utf8_lossy(&output);
        let mut heads = BTreeMap::new();
        for line in text.split('\n') {
            if line.trim().is_empty() || !line.contains('\t') {
                continue;
            }
            let Some((head_sha, reference)) = line.split_once('\t') else {
                continue;
            };
            let Some(name) = reference.strip_prefix("refs/heads/") else {
                continue;
            };
            if sha(head_sha).is_err() || name.starts_with(EPHEMERAL_BRANCH_PREFIX) {
                continue;
            }
            if validate_branch_name(name).is_err() {
                continue;
            }
            heads.insert(name.to_string(), head_sha.to_string());
        }
        Ok(heads)
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_branch_scope(
        &mut self,
        repository: &Path,
        remote: &str,
        expected: &str,
        raw: &str,
        clean: &str,
        requested_branch: Option<&str>,
        require_lease: &RequireLease,
    ) -> DeliveryResult<(Option<String>, String)> {
        // Case A: exact main verification.
        if raw == expected && clean == expected && matches!(requested_branch, None | Some("main")) {
            return Ok((Some("main".to_string()), "main".to_string()));
        }
        if requested_branch == Some("main") {
            return value_error("Non-main candidate cannot claim main source branch");
        }
        require_lease()?;
        let remote_heads = self.remote_branch_heads(repository, remote)?;
        require_lease()?;

        // Case B1: explicit requested non-main branch verified
        // against trusted remote tip/ancestry.
        if let Some(requested) = requested_branch {
            let Some(head_sha) = remote_heads.get(requested).cloned() else {
                return value_error("Requested source branch does not exist on trusted remote");
            };
            if head_sha != raw {
                if raw == expected {
                    return value_error(
                        "Requested source branch does not point at candidate commit",
                    );
                }
                self.git(
                    Some(repository),
                    &[
                        "-c",
                        "credential.helper=!/usr/bin/gh auth git-credential",
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--",
                        remote,
                        &head_sha,
                    ],
                    None,
                    None,
                    45.0,
                )?;
                require_lease()?;
                if self
                    .git(
                        Some(repository),
                        &["merge-base", "--is-ancestor", raw, &head_sha],
                        None,
                        None,
                        45.0,
                    )
                    .is_err()
                {
                    return value_error("Candidate commit is not on requested source branch");
                }
            }
            return Ok((
                Some(requested.to_string()),
                format!("branch/{}", canonical_branch_slug(requested)?),
            ));
        }

        // Case B2: unambiguous remote head match without an
        // explicit manifest source_branch.
        let matching: Vec<&String> = remote_heads
            .iter()
            .filter(|(name, head)| head.as_str() == raw && name.as_str() != "main")
            .map(|(name, _)| name)
            .collect();
        if matching.len() == 1 {
            let resolved = matching[0].clone();
            return Ok((
                Some(resolved.clone()),
                format!("branch/{}", canonical_branch_slug(&resolved)?),
            ));
        }

        // Case C: detached or ambiguous remote branch
        // metadata -> strictly non-writable scope.
        Ok((None, "detached".to_string()))
    }

    pub fn prepare_candidate(
        &mut self,
        request: &Value,
        require_lease: &RequireLease,
    ) -> DeliveryResult<Value> {
        let directory = self.directory(request)?;
        let result_path = directory.join("prepared.json");
        let protocol = request["identity_protocol"].as_str().unwrap_or("legacy-v1");
        if protocol != "legacy-v1" && protocol != "content-v2" {
            return value_error("Unknown source identity protocol");
        }
        if protocol == "content-v2" && self.identities.is_none() {
            return value_error("Content identity provider is not selected");
        }
        let requested_branch = match request.get("source_branch") {
            None | Some(Value::Null) => None,
            Some(value) => Some(validate_branch_name(value.as_str().unwrap_or_default())?),
        };
        let mut intended = json!({
            "workflow_id": request["workflow_id"],
            "project_alias": request["project_alias"],
            "source_task_id": request["source_task_id"],
            "source_assignment_id": request["source_assignment_id"],
            "source_sha256": request["source_sha256"],
            "base_revision": request["base_revision"],
            "candidate_commit": request["candidate_commit"],
            "candidate_tree": request["candidate_tree"],
            "source_branch": requested_branch,
        });
        if protocol == "content-v2" {
            intended["identity_protocol"] = json!(protocol);
        }
        let identity = sha256_hex(&canonical_bytes(&intended));
        if result_path.exists() {
            let previous = parse_json(&read_private(&result_path, 65536, Some(self.owner))?)?;
            if previous["intent_sha256"].as_str() != Some(identity.as_str()) {
                return value_error("Prepared candidate replay conflict");
            }
            if protocol == "content-v2" {
                let record =
                    read_identity_snapshot(&directory.join("identity-manifest.json"), self.owner)?;
                ExternalCandidateIdentityProvider::validate_snapshot(
                    &record["snapshot"],
                    &previous["candidate"]["content_identity"],
                )?;
                if record["intent_sha256"].as_str() != Some(identity.as_str())
                    || record["reference"] != previous["candidate"]["content_identity"]
                {
                    return value_error("Saved content preparation binding changed");
                }
            }
            return Ok(previous["candidate"].clone());
        }
        require_lease()?;
        let project_alias = request["project_alias"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let project = self.config.projects[&project_alias].clone();
        let remote = format!("https://github.com/{}.git", project.repository);
        let repository = directory.join("quarantine");
        if !repository.exists() {
            self.git(
                None,
                &["init", "--bare", &repository.to_string_lossy()],
                None,
                None,
                45.0,
            )?;
        } else if repository
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return value_error("Unsafe Git quarantine");
        }
        // The author artifact remains immutable. Freeze the
        // trusted integration separately, before any content
        // snapshot, import, review or CI effect. A retry must
        // not silently select newer main for the same action.
        let integration_path = directory.join("integration.json");
        let mut frozen: Option<ExternalSourceIntegration> = None;
        let expected: String;
        if integration_path.exists() {
            let saved = read_identity_snapshot_bounded(&integration_path, 4096, self.owner)?;
            let keys: Vec<&str> = match saved.as_object() {
                Some(map) => map.keys().map(String::as_str).collect(),
                None => return value_error("Prepared integration replay conflict"),
            };
            if keys.len() != 2
                || !keys.contains(&"intent_sha256")
                || !keys.contains(&"integration")
                || saved["intent_sha256"].as_str() != Some(identity.as_str())
            {
                return value_error("Prepared integration replay conflict");
            }
            let integration = ExternalSourceIntegration::from_value(&saved["integration"])
                .map_err(DeliveryError::value)?;
            expected = integration.integration_base.clone();
            frozen = Some(integration);
        } else if protocol == "content-v2" && directory.join("identity-manifest.json").exists() {
            // Resume pre-upgrade preparation without
            // retargeting its snapshot.
            let prior =
                read_identity_snapshot(&directory.join("identity-manifest.json"), self.owner)?;
            if prior["intent_sha256"].as_str() != Some(identity.as_str()) {
                return value_error("Prepared content replay conflict");
            }
            expected = sha(prior["integration_base"].as_str().unwrap_or_default())?;
        } else {
            expected = self.source_head(&project_alias)?;
        }
        let authored_base = sha(request["base_revision"].as_str().unwrap_or_default())?;
        let raw = sha(request["candidate_commit"].as_str().unwrap_or_default())?;
        let claimed_original = sha(request["candidate_tree"].as_str().unwrap_or_default())?;
        let mut claimed_tree = claimed_original.clone();
        self.git(
            Some(&repository),
            &[
                "-c",
                "credential.helper=!/usr/bin/gh auth git-credential",
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &remote,
                &expected,
                &raw,
                &authored_base,
            ],
            None,
            None,
            45.0,
        )?;
        require_lease()?;
        let tree_arg = format!("{raw}^{{tree}}");
        if self.git_text(
            Some(&repository),
            &["rev-parse", &tree_arg],
            None,
            None,
            45.0,
        )? != claimed_tree
        {
            return value_error("Author candidate tree mismatch");
        }
        for descendant in [&raw, &expected] {
            let merged_base = self.git_text(
                Some(&repository),
                &["merge-base", "--all", &authored_base, descendant],
                None,
                None,
                45.0,
            )?;
            if merged_base != authored_base {
                return value_error(
                    "Author candidate and trusted main must descend from the supplied base",
                );
            }
        }
        let mut prepared_tree = claimed_tree.clone();
        if expected != authored_base {
            // --stdin reports clean/conflicted status inside
            // the bounded output; a command/transport error
            // is never mistaken for a source conflict.
            let merged = self.git(
                Some(&repository),
                &[
                    "-c",
                    "core.hooksPath=/dev/null",
                    "merge-tree",
                    "--write-tree",
                    "--stdin",
                    "--no-messages",
                ],
                Some(format!("{expected} {raw}\n").as_bytes()),
                None,
                45.0,
            )?;
            let pieces: Vec<&[u8]> = merged.split(|byte| *byte == 0).collect();
            if pieces.len() < 3 || (pieces[0] != b"0" && pieces[0] != b"1") {
                return value_error("Malformed trusted merge observation");
            }
            prepared_tree = sha(std::str::from_utf8(pieces[1]).unwrap_or_default())?;
            if pieces[0] == b"0" {
                if frozen.is_some() {
                    return value_error("Frozen clean integration can no longer be reproduced");
                }
                return Ok(json!({
                    "status": "main_changed",
                    "expected_main": expected,
                    "reason": "source_conflict",
                }));
            }
            if pieces.len() != 4
                || pieces[0] != b"1"
                || pieces[1] != prepared_tree.as_bytes()
                || !pieces[2].is_empty()
                || !pieces[3].is_empty()
            {
                return value_error("Ambiguous trusted merge observation");
            }
        }
        let integration = ExternalSourceIntegration {
            prepare_action_id: request["action_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source_base_revision: authored_base.clone(),
            source_candidate_commit: raw.clone(),
            source_candidate_tree: claimed_original.clone(),
            integration_base: expected.clone(),
            candidate_tree: prepared_tree.clone(),
        };
        if let Some(frozen) = &frozen {
            if *frozen != integration {
                return value_error("Frozen source integration changed");
            }
        }
        require_lease()?;
        persist_identity_snapshot_bounded(
            &integration_path,
            &json!({"intent_sha256": identity, "integration": integration.to_value()}),
            4096,
            self.owner,
        )
        .map_err(|error| DeliveryError::value(identity_store_message(error)))?;
        claimed_tree = prepared_tree;
        let listing = self.git(
            Some(&repository),
            &["ls-tree", "-r", "-z", "--full-tree", &claimed_tree],
            None,
            None,
            45.0,
        )?;
        let mut files: BTreeMap<String, Value> = BTreeMap::new();
        let mut total: usize = 0;
        let mut rejections: Vec<(String, String)> = Vec::new();
        let mut rejected_count: i64 = 0;
        let records: Vec<&[u8]> = listing.split(|byte| *byte == 0).collect();
        for (index, record) in records.iter().enumerate() {
            if record.is_empty() {
                continue;
            }
            if index % 32 == 0 {
                require_lease()?;
            }
            let scope_error = "Links and submodules are outside external delivery scope";
            let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
                return value_error(scope_error);
            };
            let descriptor = std::str::from_utf8(&record[..tab]).unwrap_or_default();
            let path_bytes = &record[tab + 1..];
            let parts: Vec<&str> = descriptor
                .split(' ')
                .filter(|part| !part.is_empty())
                .collect();
            if parts.len() != 3 {
                return value_error(scope_error);
            }
            let (mode, kind, blob) = (parts[0], parts[1], parts[2]);
            let name = match std::str::from_utf8(path_bytes) {
                Ok(name) => name.to_string(),
                Err(_) => return value_error(scope_error),
            };
            if kind != "blob"
                || (mode != "100644" && mode != "100755")
                || index >= 20000
                || path_bytes.len() > 1024
                || name.chars().any(|ch| (ch as u32) < 32)
                || name.starts_with('/')
                || name.split('/').any(|part| matches!(part, "" | "." | ".."))
            {
                return value_error(scope_error);
            }
            let data = self.git(
                Some(&repository),
                &["cat-file", "blob", blob],
                None,
                None,
                45.0,
            )?;
            total += data.len();
            if total > 256 * 1024 * 1024 {
                return value_error("External source exceeds checkout bounds");
            }
            match artifact_guard(&name, &data) {
                Ok(()) => {
                    files.insert(name, json!([mode, blob]));
                }
                Err(error) => {
                    let code = error.code().to_string();
                    if !ARTIFACT_REJECTION_CODES.contains(&code.as_str()) {
                        return Err(DeliveryError::Publication(error));
                    }
                    rejected_count += 1;
                    if rejections.len() < 32 {
                        rejections.push((name, code));
                    }
                }
            }
        }
        if rejected_count > 0 {
            let rejected = SourceArtifactRejected::new(
                &raw,
                &integration.source_candidate_tree,
                &claimed_tree,
                rejections,
                rejected_count,
            )
            .map_err(DeliveryError::value)?;
            return Err(DeliveryError::Artifacts(Box::new(rejected)));
        }
        let base_tree = self.git_text(
            Some(&repository),
            &["rev-parse", &format!("{expected}^{{tree}}")],
            None,
            None,
            45.0,
        )?;
        let clean = if base_tree == claimed_tree {
            expected.clone()
        } else {
            let epoch: i64 = self
                .git_text(
                    Some(&repository),
                    &["show", "-s", "--format=%ct", &expected],
                    None,
                    None,
                    45.0,
                )?
                .parse::<i64>()
                .unwrap_or_default()
                + 1;
            let mut env = self.env.clone();
            env.push(("GIT_AUTHOR_DATE".to_string(), format!("@{epoch} +0000")));
            env.push(("GIT_COMMITTER_DATE".to_string(), format!("@{epoch} +0000")));
            self.git_text(
                Some(&repository),
                &["commit-tree", &claimed_tree, "-p", &expected],
                Some(
                    format!(
                        "Verified Forge workflow {}\n",
                        request["workflow_id"].as_str().unwrap_or_default()
                    )
                    .as_bytes(),
                ),
                Some(&env),
                45.0,
            )?
        };
        let (resolved_source_branch, branch_scope) = if raw == authored_base
            && clean == expected
            && matches!(requested_branch.as_deref(), None | Some("main"))
        {
            (Some("main".to_string()), "main".to_string())
        } else {
            self.resolve_branch_scope(
                &repository,
                &remote,
                &expected,
                &raw,
                &clean,
                requested_branch.as_deref(),
                require_lease,
            )?
        };
        let contract = contract_for_config(&self.config)?;
        let identity_path = directory.join("identity-manifest.json");
        let mut content_reference: Option<Value> = None;
        let (source_digest, helper_digest, gate);
        let mut gate_changes: Vec<Value> = Vec::new();
        if protocol == "content-v2" && identity_path.exists() {
            let record = read_identity_snapshot(&identity_path, self.owner)?;
            content_reference = Some(record["reference"].clone());
            let reference = content_reference.clone().unwrap();
            ExternalCandidateIdentityProvider::validate_snapshot(&record["snapshot"], &reference)?;
            if record["intent_sha256"].as_str() != Some(identity.as_str())
                || record["candidate_commit"].as_str() != Some(clean.as_str())
                || record["candidate_tree"].as_str() != Some(claimed_tree.as_str())
                || record["integration_base"].as_str() != Some(expected.as_str())
                || record["branch_scope"].as_str() != Some(branch_scope.as_str())
                || record["profile"].as_str() != Some(contract.profile.as_str())
            {
                return value_error("Persisted content preparation cannot be retargeted");
            }
            let identities = self.identities.as_mut().unwrap();
            let current = identities.artifacts.acquire(repository.as_path(), &clean)?;
            let current_base = identities
                .artifacts
                .acquire(repository.as_path(), &expected)?;
            if current.artifact_id() != reference["artifact_id"].as_str().unwrap_or_default()
                || current_base.artifact_id()
                    != reference["base_artifact_id"].as_str().unwrap_or_default()
            {
                return value_error("Persisted source content changed");
            }
            let transport = record["legacy_transport"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if transport.len() != 3 {
                return value_error("Persisted content preparation cannot be retargeted");
            }
            source_digest = transport[0].as_str().unwrap_or_default().to_string();
            helper_digest = transport[1].as_str().unwrap_or_default().to_string();
            gate = transport[2].as_str().unwrap_or_default().to_string();
            if sha256_hex(&read_private(&self.config.verifier, 1024 * 1024, Some(0))?)
                != helper_digest
            {
                return value_error(
                    "Frozen verification executor changed; create a new generation",
                );
            }
            let policy_snapshot = VerificationPolicySnapshot::parse_dict(&reference["policy"])
                .map_err(DeliveryError::value)?;
            let policy = policy_snapshot.to_dict()["policy"].clone();
            let plan = contract.plan(&branch_scope).map_err(DeliveryError::value)?;
            let mut expected_policy = ci_policy_body(&plan, &helper_digest)?;
            expected_policy["protected_artifact_id"] = policy["protected_artifact_id"].clone();
            if policy != expected_policy {
                return value_error("Frozen CI policy requires a new generation");
            }
            gate_changes = record["protected_gate_changes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
        } else {
            let scoped = contract.profile == SCOPED_PROFILE || contract.profile == EVENT_PROFILE;
            let (source, helper, gate_value) = jenkins_gate_manifest(
                &self.config,
                &files,
                if scoped {
                    Some(branch_scope.as_str())
                } else {
                    None
                },
                false,
            )?;
            source_digest = source;
            helper_digest = helper;
            gate = gate_value;
            if protocol == "content-v2" {
                let plan = contract.plan(&branch_scope).map_err(DeliveryError::value)?;
                let policy_body = ci_policy_body(&plan, &helper_digest)?;
                let generation = Uuid::parse_str(request["action_id"].as_str().unwrap_or_default())
                    .map_err(|_| DeliveryError::value("Invalid delivery UUID".to_string()))?;
                let profile = contract.profile.clone();
                let (snapshot, reference) = self.identities.as_mut().unwrap().prepare(
                    repository.as_path(),
                    &clean,
                    &expected,
                    &policy_body,
                    generation,
                    &|path: &str| {
                        content_policy_path(path, |name| protected_delivery_gate(name, &profile))
                    },
                )?;
                content_reference = Some(reference);
                let entry_map = |manifest: &Value| -> BTreeMap<String, Value> {
                    manifest["files"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|entry| {
                            (
                                entry["path"].as_str().unwrap_or_default().to_string(),
                                entry,
                            )
                        })
                        .collect()
                };
                let before = entry_map(&snapshot["base_artifact"]);
                let after = entry_map(&snapshot["artifact"]);
                let mut paths: Vec<&String> = before.keys().chain(after.keys()).collect();
                paths.sort();
                paths.dedup();
                for path in paths {
                    if before.get(path) != after.get(path)
                        && content_policy_path(path, |name| protected_delivery_gate(name, &profile))
                    {
                        gate_changes.push(json!({
                            "path": path,
                            "finding_id": format!("gate-{}", &sha256_hex(path.as_bytes())[..16]),
                        }));
                    }
                }
                require_lease()?;
                persist_identity_snapshot_bounded(
                    &identity_path,
                    &json!({
                        "intent_sha256": identity,
                        "candidate_commit": clean,
                        "candidate_tree": claimed_tree,
                        "integration_base": expected,
                        "branch_scope": branch_scope,
                        "scope": {
                            "workflow_id": request["workflow_id"],
                            "project_alias": request["project_alias"],
                            "source_task_id": request["source_task_id"],
                            "source_assignment_id": request["source_assignment_id"],
                            "source_sha256": request["source_sha256"],
                        },
                        "profile": contract.profile,
                        "snapshot": snapshot,
                        "reference": content_reference,
                        "legacy_transport": [source_digest, helper_digest, gate],
                        "protected_gate_changes": gate_changes,
                    }),
                    crate::identity_store::MAX_SNAPSHOT_BYTES,
                    self.owner,
                )
                .map_err(|error| DeliveryError::value(identity_store_message(error)))?;
            }
        }
        require_lease()?;
        let reference = format!(
            "refs/forge/external-candidates/{}",
            request["action_id"].as_str().unwrap_or_default()
        );
        self.git(
            Some(&repository),
            &["update-ref", &reference, &clean],
            None,
            None,
            45.0,
        )?;
        let bundle = directory.join("candidate.bundle");
        self.git(
            Some(&repository),
            &["bundle", "create", &bundle.to_string_lossy(), &reference],
            None,
            None,
            45.0,
        )?;
        // Git inherits its caller's umask; the trusted
        // private handoff does not.
        std::fs::set_permissions(&bundle, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|error| DeliveryError::value(error.to_string()))?;
        let canonical_repo = project.canonical_repo.clone();
        self.git(
            Some(&canonical_repo),
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--",
                &bundle.to_string_lossy(),
                &format!("{reference}:{reference}"),
            ],
            None,
            None,
            45.0,
        )?;
        let mut diff_args: Vec<&str> = vec!["diff"];
        diff_args.extend(EXTERNAL_DIFF_FLAGS.iter().copied());
        diff_args.push(&expected);
        diff_args.push(&clean);
        let patch = self.git(Some(&repository), &diff_args, None, None, 45.0)?;
        let changed = self.git(
            Some(&repository),
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--name-only",
                "-z",
                &expected,
                &clean,
            ],
            None,
            None,
            45.0,
        )?;
        if protocol == "legacy-v1" {
            gate_changes = Vec::new();
            for path in changed.split(|byte| *byte == 0) {
                if path.is_empty() {
                    continue;
                }
                let name = String::from_utf8_lossy(path).into_owned();
                if protected_delivery_gate_for(&name, &contract) {
                    gate_changes.push(json!({
                        "path": name,
                        "finding_id": format!("gate-{}", &sha256_hex(path)[..16]),
                    }));
                }
            }
        }
        let mut result = json!({
            "candidate_commit": sha(&clean)?,
            "candidate_tree": claimed_tree,
            "integration_base": expected,
            "author_candidate_commit": raw,
            "source_branch": resolved_source_branch,
            "branch_scope": branch_scope,
            "diff_sha256": sha256_hex(&patch),
            "verification_profile": contract.profile,
            "gate_source_sha256": source_digest,
            "gate_manifest_sha256": gate,
            "protected_gate_changes": gate_changes,
            "no_change": clean == expected,
            "prepare_action_id": request["action_id"],
        });
        if expected != authored_base {
            result["source_integration"] = integration.to_value();
        }
        if let Some(reference) = &content_reference {
            result["content_identity"] = reference.clone();
            result["identity_protocol"] = json!("content-v2");
        }
        durable_json(
            &result_path,
            &json!({"intent_sha256": identity, "candidate": result}),
            0o600,
        )?;
        Ok(result)
    }
}

impl TrustedDeliveryPort {
    pub fn parameters(&self, request: &Value) -> DeliveryResult<Value> {
        Ok(json!({
            "FORGE_ACTION_ID": validate_uid(request["action_id"].as_str().unwrap_or_default())?,
            "FORGE_WORKFLOW_ID": validate_uid(request["workflow_id"].as_str().unwrap_or_default())?,
            "FORGE_CANDIDATE_COMMIT": sha(request["candidate_commit"].as_str().unwrap_or_default())?,
            "FORGE_CANDIDATE_TREE": sha(request["candidate_tree"].as_str().unwrap_or_default())?,
            "FORGE_EXPECTED_MAIN": sha(request["integration_base"].as_str().unwrap_or_default())?,
            "FORGE_GATE_MANIFEST": gate_ref(request["gate_manifest_sha256"].as_str().unwrap_or_default())?,
            "FORGE_CORRELATION_ID": validate_uid(request["correlation_id"].as_str().unwrap_or_default())?,
        }))
    }

    fn assert_content_policy(
        &self,
        request: &Value,
        plan: &crate::ci_contract::CiGatePlan,
    ) -> DeliveryResult<Option<Value>> {
        let identity_path = self
            .config
            .state_root
            .join("actions")
            .join(validate_uid(
                request["prepare_action_id"].as_str().unwrap_or_default(),
            )?)
            .join("identity-manifest.json");
        let reference = request
            .get("content_identity")
            .filter(|value| !value.is_null());
        let protocol = request["identity_protocol"].as_str().unwrap_or("legacy-v1");
        if protocol != "legacy-v1" && protocol != "content-v2" {
            return value_error("Unknown CI identity protocol");
        }
        if protocol == "legacy-v1" && reference.is_none() && !identity_path.exists() {
            return Ok(None);
        }
        let Some(reference) = reference else {
            return value_error("Saved content generation cannot downgrade to legacy");
        };
        if protocol != "content-v2" {
            return value_error("Saved content generation cannot downgrade to legacy");
        }
        let record = read_identity_snapshot(&identity_path, self.owner)?;
        ExternalCandidateIdentityProvider::validate_snapshot(&record["snapshot"], reference)?;
        let expected_scope = [
            "workflow_id",
            "project_alias",
            "source_task_id",
            "source_assignment_id",
            "source_sha256",
        ];
        let scope = &record["scope"];
        let scope_keys: Vec<&str> = match scope.as_object() {
            Some(map) => map.keys().map(String::as_str).collect(),
            None => Vec::new(),
        };
        let scope_matches = scope_keys.len() == expected_scope.len()
            && expected_scope.iter().all(|key| scope_keys.contains(key))
            && expected_scope.iter().all(|key| request[key] == scope[key]);
        if reference != &record["reference"]
            || reference["verification"]["generation"].as_str()
                != request["prepare_action_id"].as_str()
            || !scope_matches
            || record["candidate_commit"].as_str() != request["candidate_commit"].as_str()
            || record["candidate_tree"].as_str() != request["candidate_tree"].as_str()
            || record["integration_base"].as_str() != request["integration_base"].as_str()
            || record["branch_scope"].as_str() != request["branch_scope"].as_str()
            || record["profile"].as_str() != Some(plan.profile.as_str())
            || request["gate_manifest_sha256"].as_str() != record["legacy_transport"][2].as_str()
        {
            return value_error("CI content generation or source scope changed");
        }
        let verifier = sha256_hex(&read_private(&self.config.verifier, 1024 * 1024, Some(0))?);
        let policy_snapshot = VerificationPolicySnapshot::parse_dict(&reference["policy"])
            .map_err(DeliveryError::value)?;
        let policy = policy_snapshot.to_dict()["policy"].clone();
        let mut expected_policy = ci_policy_body(plan, &verifier)?;
        expected_policy["protected_artifact_id"] = policy["protected_artifact_id"].clone();
        if policy != expected_policy {
            return value_error("Frozen CI policy or executor changed; create a new generation");
        }
        Ok(Some(record))
    }

    fn submission(&mut self, request: &Value) -> DeliveryResult<(PathBuf, Value)> {
        let candidate_commit = sha(request["candidate_commit"].as_str().unwrap_or_default())?;
        let expected_main = sha(request["integration_base"].as_str().unwrap_or_default())?;
        let raw_scope = request["branch_scope"].as_str();
        let raw_branch = request["source_branch"].as_str();
        if raw_scope.is_none() && raw_branch.is_none() {
            return value_error("CI submission requires a trusted branch_scope or source_branch");
        }
        let branch_scope = if let Some(scope) = raw_scope {
            let validated = crate::trusted_delivery::validate_branch_scope(
                scope,
                &candidate_commit,
                &expected_main,
            )?;
            if let Some(branch) = raw_branch {
                let expected_scope = crate::trusted_delivery::derive_branch_scope(
                    &candidate_commit,
                    &expected_main,
                    Some(branch),
                )?;
                if validated != expected_scope {
                    return value_error("CI submission branch_scope does not match source_branch");
                }
            }
            validated
        } else {
            crate::trusted_delivery::derive_branch_scope(
                &candidate_commit,
                &expected_main,
                raw_branch,
            )?
        };
        let contract = contract_for_config(&self.config)?;
        if request["verification_profile"]
            .as_str()
            .unwrap_or(crate::ci_contract::LEGACY_PROFILE)
            != contract.profile
        {
            return value_error("CI verification profile changed");
        }
        let plan = contract.plan(&branch_scope).map_err(DeliveryError::value)?;
        self.assert_content_policy(request, &plan)?;
        let prepared = self
            .config
            .state_root
            .join("actions")
            .join(validate_uid(
                request["prepare_action_id"].as_str().unwrap_or_default(),
            )?)
            .join("candidate.bundle");
        let bundle = read_private(&prepared, 128 * 1024 * 1024, Some(self.owner))?;
        let path = self.config.spool_root.join(validate_uid(
            request["action_id"].as_str().unwrap_or_default(),
        )?);
        let jenkins_gid = group_id("jenkins")
            .ok_or_else(|| DeliveryError::value("Unknown jenkins group".to_string()))?;
        if !path.exists() {
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o750)
                    .create(&path)
                    .map_err(|error| DeliveryError::value(error.to_string()))?;
            }
            std::os::unix::fs::chown(&path, Some(self.owner), Some(jenkins_gid))
                .map_err(|error| DeliveryError::value(error.to_string()))?;
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o750))
                .map_err(|error| DeliveryError::value(error.to_string()))?;
        } else if path
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return value_error("Unsafe CI submission");
        }
        let record = json!({
            "version": plan.result_version(),
            "action_id": validate_uid(request["action_id"].as_str().unwrap_or_default())?,
            "workflow_id": validate_uid(request["workflow_id"].as_str().unwrap_or_default())?,
            "project_alias": request["project_alias"],
            "candidate_commit": candidate_commit,
            "candidate_tree": sha(request["candidate_tree"].as_str().unwrap_or_default())?,
            "expected_main": expected_main,
            "branch_scope": branch_scope,
            "gate_manifest_sha256": gate_ref(request["gate_manifest_sha256"].as_str().unwrap_or_default())?,
            "bundle_sha256": sha256_hex(&bundle),
            "verification_profile": contract.profile,
            "correlation_id": validate_uid(request["correlation_id"].as_str().unwrap_or_default())?,
        });
        let request_path = path.join("request.json");
        if request_path.exists() {
            let existing = parse_json(&read_private(&request_path, 65536, Some(self.owner))?)?;
            if existing != record {
                return value_error("CI submission identity conflict");
            }
        } else {
            let bundle_path = path.join("candidate.bundle");
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut options = std::fs::OpenOptions::new();
                options
                    .write(true)
                    .create_new(true)
                    .custom_flags(0o400000)
                    .mode(0o440);
                let mut file = options
                    .open(&bundle_path)
                    .map_err(|error| DeliveryError::value(error.to_string()))?;
                std::os::unix::fs::chown(&bundle_path, Some(self.owner), Some(jenkins_gid))
                    .map_err(|error| DeliveryError::value(error.to_string()))?;
                file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o440))
                    .map_err(|error| DeliveryError::value(error.to_string()))?;
                file.write_all(&bundle)
                    .and_then(|_| file.sync_all())
                    .map_err(|error| DeliveryError::value(error.to_string()))?;
            }
            crate::durable_files::durable_json_group(
                &request_path,
                &record,
                Some(jenkins_gid),
                0o440,
            )?;
        }
        Ok((path, record))
    }

    pub fn verify(
        &mut self,
        request: &Value,
        require_lease: &RequireLease,
    ) -> DeliveryResult<Value> {
        let instance = self.config.jenkins_instance.clone();
        let directory = self.directory(request)?;
        let parameters = self.parameters(request)?;
        let (_submission, record) = self.submission(request)?;
        let journal = directory.join("jenkins.json");
        let previous: Option<Value> = if journal.exists() {
            Some(parse_json(&read_private(
                &journal,
                65536,
                Some(self.owner),
            )?)?)
        } else {
            None
        };
        if let Some(previous) = &previous {
            if previous["parameters"] != parameters {
                return value_error("Jenkins replay intent changed");
            }
        }
        require_lease()?;
        let mut receipt = record.clone();
        receipt["provider"] = json!("jenkins");
        receipt["job"] = json!(self.config.jenkins_job);
        let found = match self.jenkins.find(&parameters) {
            Ok(found) => found,
            Err(DeliveryError::Unavailable(_)) => {
                return Ok(json!({
                    "status": "pending",
                    "receipt": receipt,
                    "summary": "Jenkins observation is unavailable; preserving the same action without resubmission.",
                }));
            }
            Err(error) => return Err(error),
        };
        let Some((kind, value)) = found else {
            if previous.is_some() {
                let mut reconciling = receipt.clone();
                reconciling["submission"] = json!("reconciling");
                return Ok(json!({
                    "status": "pending",
                    "receipt": reconciling,
                    "summary": "Reconciling the durable Jenkins submission; no duplicate was sent.",
                }));
            }
            let mut journal_record = json!({"parameters": parameters, "post_started": true});
            if let Some(instance) = &instance {
                journal_record["jenkins_instance"] = json!(instance);
            }
            durable_json(&journal, &journal_record, 0o600)?;
            require_lease()?;
            match self.jenkins.submit(&parameters) {
                Ok(number) => {
                    journal_record["queue_id"] = json!(number);
                    durable_json(&journal, &journal_record, 0o600)?;
                    receipt["queue_id"] = json!(number);
                }
                Err(DeliveryError::Publication(_)) | Err(DeliveryError::Value(_)) => {
                    receipt["submission"] = json!("uncertain");
                }
                Err(error) => return Err(error),
            }
            return Ok(json!({
                "status": "pending",
                "receipt": receipt,
                "summary": "Jenkins verification submitted; awaiting its durable build record.",
            }));
        };
        if kind == "queue" {
            receipt["queue_id"] = value["id"].clone();
            return Ok(json!({
                "status": "pending",
                "receipt": receipt,
                "summary": "Jenkins verification is queued.",
            }));
        }
        receipt["build_number"] = value["number"].clone();
        receipt["queue_id"] = value["queue_id"].clone();
        // First build observation for the action: the
        // journaled build number becomes the trusted binding.
        // Later observations must equal it; the producer
        // enforces that.
        let mut journal_record = match &previous {
            Some(previous) => previous.clone(),
            None => json!({"parameters": parameters, "post_started": true}),
        };
        if let Some(instance) = &instance {
            if journal_record.get("jenkins_instance").is_none() {
                journal_record["jenkins_instance"] = json!(instance);
            }
        }
        if journal_record["build_number"] != value["number"]
            || journal_record["queue_id"] != value["queue_id"]
        {
            journal_record["build_number"] = value["number"].clone();
            journal_record["queue_id"] = value["queue_id"].clone();
            durable_json(&journal, &journal_record, 0o600)?;
        }
        if value["building"] == Value::Bool(true) {
            return Ok(json!({
                "status": "pending",
                "receipt": receipt,
                "summary": "Jenkins is running the fixed verification gates.",
            }));
        }
        receipt["result"] = value["result"].clone();
        require_lease()?;
        let result_file = self.config.results_root.join(format!(
            "{}.json",
            request["action_id"].as_str().unwrap_or_default()
        ));
        if !result_file.exists() {
            receipt["jenkins_result"] = value["result"].clone();
            return Ok(json!({
                "status": "failed",
                "receipt": receipt,
                "summary": "The finished Jenkins build has no trusted verification receipt.",
            }));
        }
        let raw = read_private(&result_file, 262144, Some(0))?;
        let result = parse_json(&raw)?;
        for key in [
            "action_id",
            "workflow_id",
            "candidate_commit",
            "candidate_tree",
            "expected_main",
            "branch_scope",
            "gate_manifest_sha256",
            "verification_profile",
            "correlation_id",
        ] {
            if result[key] != record[key] {
                return value_error("Trusted CI receipt source mismatch");
            }
        }
        let mut expected_hash = canonical_bytes(&record);
        expected_hash.push(b'\n');
        if result["request_sha256"].as_str() != Some(sha256_hex(&expected_hash).as_str()) {
            return value_error("Trusted CI receipt source mismatch");
        }
        let check_receipt_sha256 = sha256_hex(&raw);
        receipt["check_receipt_sha256"] = json!(check_receipt_sha256);
        receipt["cleanup_confirmed"] = json!(result["cleanup_confirmed"] == Value::Bool(true));
        receipt["checks"] = result["checks"].clone();
        receipt["log_sha256"] = result["log_sha256"].clone();
        receipt["failure"] = result["failure"].clone();
        if result["cache"].is_object() {
            receipt["cache"] = result["cache"].clone();
        }
        if result["stages"].is_array() {
            receipt["stages"] = result["stages"].clone();
        }
        let checks = result["checks"].clone();
        let plan = contract_for_config(&self.config)?
            .plan(record["branch_scope"].as_str().unwrap_or_default())
            .map_err(DeliveryError::value)?;
        if plan.profile == SCOPED_PROFILE || plan.profile == EVENT_PROFILE {
            receipt["verification_profile"] = result["verification_profile"].clone();
            receipt["result_version"] = result["version"].clone();
            receipt["coverage"] = result["coverage"].clone();
        }
        if plan.profile == EVENT_PROFILE && result["cleanup_confirmed"] == Value::Bool(true) {
            // Only the protected result file can supply
            // fixed-phase witnesses. Copy exact validated
            // fields; never classify a failure from logs.
            let witness = crate::verification::protected_retry_observation(
                &plan.profile,
                &plan.scope,
                Some(&result),
                Some(&check_receipt_sha256),
            )?;
            for key in ["failure_kind", "passed_kinds", "phase_results"] {
                receipt[key] = result[key].clone();
            }
            receipt["retry_observation"] = witness.unwrap_or(Value::Null);
        }
        let complete = result["version"].as_i64().is_some()
            && result["version"].as_i64() == Some(plan.result_version())
            && (plan.coverage().is_none() || result["coverage"] == plan.coverage().unwrap())
            && checks.is_array()
            && checks.as_array().map(|items| items.len()) == Some(plan.checks.len())
            && checks
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .all(|(index, check)| {
                    *check
                        == json!({
                            "index": index,
                            "argv": plan.checks[index],
                            "returncode": 0,
                        })
                });
        if value["result"].as_str() == Some("SUCCESS")
            && result["status"].as_str() == Some("passed")
            && result["cleanup_confirmed"] == Value::Bool(true)
            && complete
        {
            receipt["result"] = json!("SUCCESS");
            receipt["jenkins_result"] = value["result"].clone();
            let summary = if plan.profile == crate::ci_contract::LEGACY_PROFILE {
                "All fixed Jenkins gates passed on the exact reviewed candidate."
            } else if plan.scope == "branch" {
                "Exact branch in-memory gates passed; browser/database were not run."
            } else {
                "Exact main gates passed, including real browser and all four zero-skip backend suites."
            };
            return Ok(json!({
                "status": "passed",
                "receipt": receipt,
                "summary": summary,
            }));
        }
        receipt["jenkins_result"] = value["result"].clone();
        let failure_text = match result.get("failure") {
            None => "checks_failed".to_string(),
            Some(Value::Null) => "None".to_string(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        let failure_text: String = failure_text.chars().take(128).collect();
        let mut summary = format!("Jenkins verification failed: {failure_text}");
        if let Some(items) = checks.as_array() {
            for check in items {
                let returncode = &check["returncode"];
                if !returncode.is_null() && returncode.as_i64() != Some(0) {
                    summary += &format!("; fixed check {}, exit {}", check["index"], returncode);
                }
            }
        }
        let log_path = self.config.results_root.join(format!(
            "{}.log",
            request["action_id"].as_str().unwrap_or_default()
        ));
        if log_path.exists() {
            let raw_log = read_private(&log_path, 64 * 1024 * 1024, Some(0))?;
            if sha256_hex(&raw_log) != result["log_sha256"].as_str().unwrap_or_default() {
                return value_error("Trusted CI log digest changed");
            }
            let tail = &raw_log[raw_log.len().saturating_sub(3072)..];
            let decoded = String::from_utf8_lossy(tail);
            let mut excerpt: String = decoded
                .chars()
                .filter(|ch| *ch == '\n' || *ch == '\t' || (*ch as u32) >= 32)
                .collect();
            if artifact_guard("ci-diagnostic.txt", excerpt.as_bytes()).is_err() {
                excerpt = "The bounded diagnostic was withheld by the artifact guard.".to_string();
            }
            summary += "\n";
            summary += &excerpt;
        }
        let summary: String = summary.chars().take(4096).collect();
        Ok(json!({
            "status": "failed",
            "receipt": receipt,
            "summary": summary,
        }))
    }

    pub fn merge(
        &mut self,
        request: &Value,
        require_lease: &RequireLease,
    ) -> DeliveryResult<Value> {
        let project = request["project_alias"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let candidate = sha(request["candidate_commit"].as_str().unwrap_or_default())?;
        let expected = sha(request["integration_base"].as_str().unwrap_or_default())?;
        let tree = sha(request["candidate_tree"].as_str().unwrap_or_default())?;
        require_lease()?;
        let current = self.source_head(&project)?;
        let receipt = json!({
            "candidate_commit": candidate,
            "candidate_tree": tree,
            "expected_main": expected,
            "merge_commit": candidate,
            "workflow_id": validate_uid(request["workflow_id"].as_str().unwrap_or_default())?,
        });
        let verification = &request["verification"];
        if verification["result"].as_str() != Some("SUCCESS")
            || verification["candidate_commit"].as_str() != Some(candidate.as_str())
            || verification["candidate_tree"].as_str() != Some(tree.as_str())
            || verification["gate_manifest_sha256"] != request["gate_manifest_sha256"]
        {
            return value_error("Merge lacks an exact-source successful verification");
        }
        if current == candidate {
            return Ok(json!({
                "status": "merged",
                "receipt": receipt,
                "summary": "The exact verified candidate is already main.",
            }));
        }
        let project_config = self.config.projects[&project].clone();
        let repository = project_config.canonical_repo.clone();
        let remote = format!("https://github.com/{}.git", project_config.repository);
        if current != expected {
            self.git(
                Some(&repository),
                &[
                    "-c",
                    "credential.helper=!/usr/bin/gh auth git-credential",
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--",
                    &remote,
                    &current,
                ],
                None,
                None,
                45.0,
            )?;
            match self.git(
                Some(&repository),
                &["merge-base", "--is-ancestor", &candidate, &current],
                None,
                None,
                45.0,
            ) {
                Err(DeliveryError::Publication(_)) => {
                    let mut changed = receipt.clone();
                    changed["observed_main"] = json!(current);
                    return Ok(json!({
                        "status": "main_changed",
                        "receipt": changed,
                        "summary": "Main advanced; the candidate needs fresh integration and verification.",
                    }));
                }
                Err(error) => return Err(error),
                Ok(_) => {
                    let mut merged = receipt.clone();
                    merged["observed_main"] = json!(current);
                    return Ok(json!({
                        "status": "merged",
                        "receipt": merged,
                        "summary": "The exact verified candidate is already in the current main history.",
                    }));
                }
            }
        }
        // Only the sanitized commit and its already-public
        // main ancestry leave the quarantine. The raw
        // external author's private history never does.
        let observed_tree = self.git_text(
            Some(&repository),
            &["rev-parse", &format!("{candidate}^{{tree}}")],
            None,
            None,
            45.0,
        )?;
        if observed_tree != tree {
            return value_error("Local publication tree changed");
        }
        let reference = format!(
            "refs/heads/forge/external-candidate-{}",
            validate_uid(request["prepare_action_id"].as_str().unwrap_or_default())?
        );
        require_lease()?;
        self.git(
            Some(&repository),
            &[
                "-c",
                "credential.helper=!/usr/bin/gh auth git-credential",
                "push",
                "--porcelain",
                "--no-verify",
                "--",
                &remote,
                &format!("{candidate}:{reference}"),
            ],
            None,
            None,
            45.0,
        )?;
        require_lease()?;
        let observed = self.api(&project, "GET", &format!("git/commits/{candidate}"), None)?;
        if observed["sha"].as_str() != Some(candidate.as_str())
            || observed["tree"]["sha"].as_str() != Some(tree.as_str())
        {
            return value_error("Remote candidate identity changed");
        }
        require_lease()?;
        if self.source_head(&project)? != expected {
            return Ok(json!({
                "status": "main_changed",
                "receipt": receipt,
                "summary": "Main advanced during candidate publication; fresh review and checks are required.",
            }));
        }
        require_lease()?;
        self.api(
            &project,
            "PATCH",
            "git/refs/heads/main",
            Some(&json!({"sha": candidate, "force": false})),
        )?;
        if self.source_head(&project)? != candidate {
            return Ok(json!({
                "status": "pending",
                "receipt": receipt,
                "summary": "Reconciling the guarded main update.",
            }));
        }
        Ok(json!({
            "status": "merged",
            "receipt": receipt,
            "summary": "The independently reviewed and Jenkins-verified candidate was merged.",
        }))
    }

    pub fn cancel(
        &mut self,
        request: &Value,
        require_lease: &RequireLease,
    ) -> DeliveryResult<Value> {
        let mut target = request["target_request"].clone();
        target["action_id"] = json!(validate_uid(
            request["target_action_id"].as_str().unwrap_or_default()
        )?);
        target["workflow_id"] = json!(validate_uid(
            request["workflow_id"].as_str().unwrap_or_default()
        )?);
        target["project_alias"] = request["project_alias"].clone();
        if request["target_kind"].as_str() != Some("verify") {
            return value_error("Only a Jenkins verification can require process cancellation");
        }
        let submission = self
            .config
            .spool_root
            .join(target["action_id"].as_str().unwrap_or_default());
        if submission.exists() {
            if submission
                .symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
            {
                return value_error("Unsafe cancellation spool");
            }
            durable_json(
                &submission.join("cancelled"),
                &json!({"cancel_action_id": validate_uid(request["action_id"].as_str().unwrap_or_default())?}),
                0o600,
            )?;
        }
        require_lease()?;
        let parameters = self.parameters(&target)?;
        let found = self.jenkins.find(&parameters)?;
        let mut receipt = json!({
            "provider": "jenkins",
            "target_action_id": target["action_id"],
        });
        let Some((kind, value)) = found else {
            // A journaled POST can be delayed or have lost
            // its acknowledgement. Keep the cancellation
            // marker and wait for an actual observation.
            let journal = self
                .config
                .state_root
                .join("actions")
                .join(target["action_id"].as_str().unwrap_or_default())
                .join("jenkins.json");
            if journal.exists() {
                let prior = parse_json(&read_private(&journal, 65536, Some(self.owner))?)?;
                let queue_id = if prior
                    .as_object()
                    .map(|object| object.contains_key("queue_id"))
                    .unwrap_or(false)
                {
                    prior["queue_id"].clone()
                } else {
                    request["target_receipt"]["queue_id"].clone()
                };
                if !queue_id.is_null() {
                    let queue_text = match &queue_id {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    let observation =
                        self.jenkins
                            .call("queue", std::slice::from_ref(&queue_text), false)?;
                    if observation["queue_id"].as_str() == Some(queue_text.as_str())
                        && observation["cancelled"].as_str() == Some("true")
                        && observation["build_number"].as_str() == Some("not_reported")
                    {
                        let mut cancelled = receipt.clone();
                        cancelled["queue_id"] = queue_id;
                        cancelled["process_started"] = json!(false);
                        return Ok(json!({
                            "status": "cancelled",
                            "receipt": cancelled,
                            "summary": "Jenkins confirmed queue cancellation before an executable was assigned.",
                        }));
                    }
                }
                return Ok(json!({
                    "status": "pending",
                    "receipt": receipt,
                    "summary": "Reconciling the cancelled submission before confirming process shutdown.",
                }));
            }
            receipt["process_started"] = json!(false);
            return Ok(json!({
                "status": "cancelled",
                "receipt": receipt,
                "summary": "The verification was cancelled before any Jenkins submission.",
            }));
        };
        if kind == "queue" {
            self.jenkins
                .stop(Some(value["id"].as_i64().unwrap_or_default()), None)?;
            let mut queued = receipt.clone();
            queued["queue_id"] = value["id"].clone();
            return Ok(json!({
                "status": "pending",
                "receipt": queued,
                "summary": "Jenkins queue cancellation requested; awaiting confirmation.",
            }));
        }
        receipt["build_number"] = value["number"].clone();
        if value["building"] == Value::Bool(true) {
            self.jenkins
                .stop(None, Some(value["number"].as_i64().unwrap_or_default()))?;
            return Ok(json!({
                "status": "pending",
                "receipt": receipt,
                "summary": "Jenkins process stop requested; awaiting the trusted cleanup receipt.",
            }));
        }
        let result_path = self.config.results_root.join(format!(
            "{}.json",
            target["action_id"].as_str().unwrap_or_default()
        ));
        if result_path.exists() {
            let raw = read_private(&result_path, 262144, Some(0))?;
            let result = parse_json(&raw)?;
            if result["action_id"] == target["action_id"]
                && result["cleanup_confirmed"] == Value::Bool(true)
            {
                let mut cancelled = receipt.clone();
                cancelled["cleanup_confirmed"] = json!(true);
                cancelled["check_receipt_sha256"] = json!(sha256_hex(&raw));
                return Ok(json!({
                    "status": "cancelled",
                    "receipt": cancelled,
                    "summary": "Jenkins stopped and its trusted supervisor confirmed all build processes were reaped.",
                }));
            }
        }
        Ok(json!({
            "status": "pending",
            "receipt": receipt,
            "summary": "Jenkins stopped; waiting for the trusted process cleanup receipt.",
        }))
    }
}

/// The delivery provider factory: validates the Jenkins
/// client contract and the pinned binary, verifies verifier
/// policy parity where required, asserts the observer
/// scope, and composes the trusted port with its content
/// identity provider.
pub struct DeliveryProviderFactory {
    pub config: DeliveryConfig,
    pub observer: Option<Box<dyn crate::trusted_delivery::JenkinsObserverPort>>,
}

impl DeliveryProviderFactory {
    pub fn create(
        self,
        jenkins_execute: Box<dyn CommandExecutor>,
        execute: Box<dyn CommandExecutor>,
        git_port: std::sync::Arc<dyn crate::artifact_source::GitArtifactGitPort + Send + Sync>,
    ) -> DeliveryResult<TrustedDeliveryPort> {
        let selected = parse_json(&read_private(
            &self.config.jenkins_config,
            65536,
            Some(effective_uid()),
        )?)?;
        let parameters: Vec<Value> = PARAMETERS.iter().map(|item| json!(item)).collect();
        if selected["endpoint"].as_str() != Some("http://127.0.0.1:18084/")
            || selected["allow_mutations"] != Value::Bool(true)
            || selected["observed_parameters"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                != parameters
        {
            return value_error(
                "Jenkins must explicitly expose the complete public correlation contract",
            );
        }
        if sha256_hex(&read_private(
            &self.config.jenkins_binary,
            64 * 1024 * 1024,
            None,
        )?) != self.config.jenkins_binary_sha256
        {
            return value_error("Pinned Jenkins executable changed");
        }
        let contract = contract_for_config(&self.config)?;
        let verifier_exists = self.config.verifier.exists()
            || self
                .config
                .verifier
                .symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false);
        if contract.profile == SCOPED_PROFILE
            || contract.profile == EVENT_PROFILE
            || verifier_exists
        {
            let verifier_source = read_private(&self.config.verifier, 1024 * 1024, Some(0))?;
            crate::verifier_parity::verify_verifier_policy_parity_contract(
                &contract,
                &verifier_source,
                &PythonAstPort,
            )?;
        }
        let mut observer = self.observer;
        if let Some(observer) = observer.as_mut() {
            observer
                .assert_scope(
                    &self.config.jenkins_job,
                    selected["endpoint"].as_str().unwrap_or_default(),
                    &PARAMETERS
                        .iter()
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| {
                    let message = match error {
                        crate::jenkins_observer::JenkinsObserverError::Unavailable(message)
                        | crate::jenkins_observer::JenkinsObserverError::Invalid(message)
                        | crate::jenkins_observer::JenkinsObserverError::Execute(message)
                        | crate::jenkins_observer::JenkinsObserverError::Io(message) => message,
                    };
                    DeliveryError::value(message)
                })?;
        }
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.config.state_root)
                .map_err(|error| DeliveryError::value(error.to_string()))?;
        }
        let jenkins = ForgeJenkinsPort::new(self.config.clone(), jenkins_execute, observer);
        let mut delivery = TrustedDeliveryPort::new(self.config, jenkins, execute, None);
        delivery.identities = Some(ExternalCandidateIdentityProvider {
            artifacts: Box::new(crate::external_identity::GitArtifactsProvider {
                git: git_port,
                limits: crate::artifact_source::SourceLimits::default(),
            }),
        });
        Ok(delivery)
    }
}

/// The external delivery worker: claims durable actions and
/// dispatches them to the trusted port under the lease
/// fence, with the persistent agent-branch reservation held
/// across prepare/verify/merge/cancel. The run loop and
/// process entry are host concerns (S7). The store sits
/// behind a RefCell because the lease fence and the dispatch
/// both need it, one call at a time (single-threaded
/// worker, as in Python).
pub struct ExternalDeliveryWorker {
    pub dao: std::rc::Rc<std::cell::RefCell<Box<dyn ExternalDeliveryStorePort>>>,
    pub delivery: TrustedDeliveryPort,
    pub worker_id: String,
    pub stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ExternalDeliveryWorker {
    fn lease_check(
        dao: &std::rc::Rc<std::cell::RefCell<Box<dyn ExternalDeliveryStorePort>>>,
        action: &ExternalDeliveryLease,
        stop: &std::sync::atomic::AtomicBool,
    ) -> DeliveryResult<()> {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DeliveryError::LeaseLost);
        }
        match dao.borrow_mut().renew(action) {
            Ok(Value::Bool(true)) => Ok(()),
            Ok(_) => Err(DeliveryError::LeaseLost),
            // A store failure while renewing abandons the
            // action to its lease lapse rather than forging
            // ahead without a lease (Python propagates the
            // store exception to the run loop; the observable
            // outcome — no checkpoint under a dead lease —
            // is the same).
            Err(_) => Err(DeliveryError::LeaseLost),
        }
    }

    pub fn run_once(&mut self) -> Result<bool, String> {
        let Some(action) = self
            .dao
            .borrow_mut()
            .claim(&self.worker_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let dao = self.dao.clone();
        let stop_flag = self.stop.clone();
        let action_copy = action.clone();
        let require = move || -> DeliveryResult<()> {
            ExternalDeliveryWorker::lease_check(&dao, &action_copy, &stop_flag)
        };
        require().map_err(|_| "LeaseLost".to_string())?;
        let mut request = action.request.clone();
        request["action_id"] = json!(action.action_id);
        request["workflow_id"] = json!(action.workflow_id);
        request["project_alias"] = json!(action.project_alias);
        let agent_profile = request["agent_profile"].as_str().map(str::to_string);
        let mut branch_reservation: Option<Value> = None;
        let mut branch_name_selected: Option<String> = None;
        if let Some(profile) = &agent_profile {
            if ["prepare", "verify", "merge", "cancel"].contains(&action.kind.as_str()) {
                let selected = request["source_branch"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        request["expected_agent_branch"]
                            .as_str()
                            .map(str::to_string)
                    })
                    .or_else(|| {
                        forge_work_worker::config_profile::default_agent_branch(profile).ok()
                    });
                if let Some(selected) = selected {
                    if selected != "main" {
                        let scope =
                            forge_work_worker::agent_branch::branch_scope_for_branch(&selected)
                                .map_err(|error| error.to_string())?;
                        let root_task_id = request["root_task_id"]
                            .as_str()
                            .or_else(|| request["source_task_id"].as_str())
                            .unwrap_or(&action.workflow_id)
                            .to_string();
                        let task_id = request["source_task_id"]
                            .as_str()
                            .unwrap_or(&root_task_id)
                            .to_string();
                        branch_name_selected = Some(selected.clone());
                        let acquired = self
                            .dao
                            .borrow_mut()
                            .agent_branch_acquire(
                                &action.project_alias,
                                profile,
                                &selected,
                                &scope,
                                &action.workflow_id,
                                &root_task_id,
                                &task_id,
                            )
                            .map_err(|error| error.to_string())?;
                        if acquired["acquired"] != Value::Bool(true) {
                            let result = json!({
                                "status": "pending",
                                "receipt": request["previous_receipt"].clone(),
                                "summary": "Persistent agent branch is currently owned by another active workflow; waiting.",
                            });
                            require().map_err(|_| "LeaseLost".to_string())?;
                            self.dao
                                .borrow_mut()
                                .checkpoint(&action, &result)
                                .map_err(|error| error.to_string())?;
                            return Ok(true);
                        }
                        branch_reservation = Some(acquired);
                    }
                }
            }
        }
        let outcome: DeliveryResult<Value> = (|| match action.kind.as_str() {
            "source" => {
                let head = self.delivery.source_head(&action.project_alias)?;
                Ok(json!({
                    "status": "passed",
                    "receipt": {"base_revision": head},
                    "summary": "Current configured source head observed.",
                }))
            }
            "prepare" => {
                let candidate = self.delivery.prepare_candidate(&request, &require)?;
                let status = candidate["status"].as_str().unwrap_or("passed").to_string();
                let summary = if candidate["reason"].as_str() == Some("source_conflict") {
                    "Trusted source integration found conflicts; the preserved candidate needs its original author."
                } else {
                    "Exact external candidate prepared for independent review."
                };
                let result = json!({
                    "status": status,
                    "receipt": candidate.clone(),
                    "summary": summary,
                });
                if branch_reservation.is_some() && status == "passed" {
                    let resolved = candidate["source_branch"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| branch_name_selected.clone());
                    if let Some(resolved) = resolved {
                        if resolved != "main" {
                            let scope =
                                forge_work_worker::agent_branch::branch_scope_for_branch(&resolved)
                                    .map_err(|error| DeliveryError::value(error.to_string()))?;
                            self.dao
                                .borrow_mut()
                                .agent_branch_upsert(
                                    &action.project_alias,
                                    agent_profile.as_deref().unwrap_or_default(),
                                    &resolved,
                                    &scope,
                                    &BranchUpsert {
                                        head_commit_sha: candidate["candidate_commit"]
                                            .as_str()
                                            .map(str::to_string),
                                        fetched_main_sha: candidate["integration_base"]
                                            .as_str()
                                            .map(str::to_string),
                                        fetched_main_at: Some(
                                            crate::publication_clients::utc_now_iso(),
                                        ),
                                        sync_status: Some("synced_with_main".to_string()),
                                        conflict_paths: Some(Vec::new()),
                                        cache_status: None,
                                        last_task_id: request["source_task_id"]
                                            .as_str()
                                            .map(str::to_string),
                                        last_correlation_id: request["correlation_id"]
                                            .as_str()
                                            .map(str::to_string),
                                    },
                                )
                                .map_err(|error| DeliveryError::value(error.to_string()))?;
                        }
                    }
                }
                Ok(result)
            }
            "verify" => {
                let result = self.delivery.verify(&request, &require)?;
                if branch_reservation.is_some() {
                    let cache = &result["receipt"]["cache"];
                    let cache_status = if cache.is_object() {
                        let raw = cache["restore_status"]
                            .as_str()
                            .or_else(|| cache["status"].as_str());
                        match raw {
                            Some("hit_main_seed") => Some("hit_main".to_string()),
                            Some(status @ ("hit_branch" | "hit_main" | "miss" | "disabled")) => {
                                Some(status.to_string())
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if let (Some(status), Some(selected)) = (cache_status, &branch_name_selected) {
                        let scope =
                            forge_work_worker::agent_branch::branch_scope_for_branch(selected)
                                .map_err(|error| DeliveryError::value(error.to_string()))?;
                        self.dao
                            .borrow_mut()
                            .agent_branch_upsert(
                                &action.project_alias,
                                agent_profile.as_deref().unwrap_or_default(),
                                selected,
                                &scope,
                                &BranchUpsert {
                                    cache_status: Some(status),
                                    last_task_id: request["source_task_id"]
                                        .as_str()
                                        .map(str::to_string),
                                    last_correlation_id: request["correlation_id"]
                                        .as_str()
                                        .map(str::to_string),
                                    ..Default::default()
                                },
                            )
                            .map_err(|error| DeliveryError::value(error.to_string()))?;
                    }
                }
                Ok(result)
            }
            "merge" => {
                if let Some(reservation) = &branch_reservation {
                    let epoch = reservation["reservation_epoch"]
                        .as_i64()
                        .unwrap_or_default();
                    let check = self
                        .dao
                        .borrow_mut()
                        .agent_branch_verify_owner(
                            &action.project_alias,
                            agent_profile.as_deref().unwrap_or_default(),
                            &action.workflow_id,
                            epoch,
                        )
                        .map_err(|error| DeliveryError::value(error.to_string()))?;
                    if check["valid"] != Value::Bool(true) {
                        return value_error(
                            "Persistent branch ownership lost before external merge",
                        );
                    }
                }
                let result = self.delivery.merge(&request, &require)?;
                if let Some(reservation) = branch_reservation
                    .as_ref()
                    .filter(|_| result["status"].as_str() == Some("merged"))
                {
                    let epoch = reservation["reservation_epoch"]
                        .as_i64()
                        .unwrap_or_default();
                    self.dao
                        .borrow_mut()
                        .agent_branch_release(
                            &action.project_alias,
                            agent_profile.as_deref().unwrap_or_default(),
                            &action.workflow_id,
                            epoch,
                            "merged",
                            true,
                            result["receipt"]["merge_commit"].as_str(),
                        )
                        .map_err(|error| DeliveryError::value(error.to_string()))?;
                }
                Ok(result)
            }
            "cancel" => {
                let result = self.delivery.cancel(&request, &require)?;
                if let Some(reservation) = branch_reservation
                    .as_ref()
                    .filter(|_| result["status"].as_str() == Some("cancelled"))
                {
                    let epoch = reservation["reservation_epoch"]
                        .as_i64()
                        .unwrap_or_default();
                    self.dao
                        .borrow_mut()
                        .agent_branch_release(
                            &action.project_alias,
                            agent_profile.as_deref().unwrap_or_default(),
                            &action.workflow_id,
                            epoch,
                            "cancelled",
                            true,
                            None,
                        )
                        .map_err(|error| DeliveryError::value(error.to_string()))?;
                }
                Ok(result)
            }
            _ => value_error("Unknown delivery action"),
        })();
        let result = match outcome {
            Ok(result) => result,
            Err(DeliveryError::Artifacts(error)) => {
                if action.kind != "prepare"
                    || error.candidate_commit
                        != request["candidate_commit"].as_str().unwrap_or_default()
                    || error.candidate_tree
                        != request["candidate_tree"].as_str().unwrap_or_default()
                {
                    return Err(
                        "Source rejection does not match its claimed preparation".to_string()
                    );
                }
                error.result()
            }
            Err(DeliveryError::Unavailable(_)) | Err(DeliveryError::Publication(_)) => json!({
                "status": "pending",
                "receipt": request["previous_receipt"].clone(),
                "summary": "Trusted delivery observation is temporarily unavailable; retaining the same bounded action.",
            }),
            Err(DeliveryError::Value(_)) => json!({
                "status": "failed",
                "receipt": {},
                "summary": "Trusted delivery failed: ValueError",
            }),
            Err(DeliveryError::LeaseLost) => return Err("LeaseLost".to_string()),
        };
        require().map_err(|_| "LeaseLost".to_string())?;
        self.dao
            .borrow_mut()
            .checkpoint(&action, &result)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }
}
