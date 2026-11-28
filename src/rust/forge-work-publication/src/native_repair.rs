//! Parent-owned, explicitly unapproved Git sources for
//! bounded delivery repair (work_native_repair.py's
//! BranchFirstRepairProvider): the F7 RepairSourcesPort
//! implementation plus the merge-side prepare().
//!
//! A repair seed is never a passed candidate or a main
//! update. Its Git parents identify the tested source and
//! observed main, while its tree preserves the real merge's
//! nonconflicting files and unresolved conflict markers.
//!
//! The migration map assigns work_native_repair.py to the
//! worker slice, but the provider's dependencies (the
//! publication git client, the publication config, the gate
//! manifest) layer it here; the trait it implements lives in
//! forge-work-worker.

use crate::durable_files::{durable_json, read_private};
use crate::publication_clients::ProviderError;
use crate::publication_clients::{GitRepositoryProvider, PublicationError};
use crate::publication_config::PublicationConfig;
use forge_work::canonical::{canonical_json, sha256_hex};
use forge_work_worker::review_candidate::ReviewedCandidateProvider;
use forge_work_worker::review_job::gate_source_digest;
use forge_work_worker::review_ports::{RepairSourcesPort, ReviewError};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use uuid::Uuid;

type RepairResult<T> = Result<T, ReviewError>;

fn code<T>(code: &str) -> RepairResult<T> {
    Err(ReviewError::from(PublicationError::new(code)))
}

fn text_of<'a>(job: &'a Value, key: &str) -> &'a str {
    job.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// work_external_delivery.sha: a 40-hex commit string or a
/// plain ValueError.
fn sha(value: &str) -> RepairResult<String> {
    let valid = value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(ReviewError::other("Invalid delivery commit"));
    }
    Ok(value.to_string())
}

fn parse_private_json(path: &Path) -> RepairResult<Value> {
    let raw = read_private(path, 65536, None).map_err(ReviewError::other)?;
    let text = std::str::from_utf8(&raw).map_err(|error| ReviewError::other(error.to_string()))?;
    crate::strict_json::strict_parse(text.as_bytes())
        .map_err(|error| ReviewError::other(error.to_string()))
}

fn write_durable(path: &Path, value: &Value) -> RepairResult<()> {
    durable_json(path, value, 0o600).map_err(ReviewError::other)
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

/// The native-CI build reference the merge-side prepare
/// embeds (work_native_jenkins' build_reference).
pub trait RepairNativeCiPort: Send {
    fn build_reference(&mut self, job: &Value) -> RepairResult<Value>;
}

/// BranchFirstRepairProvider.
pub struct BranchFirstRepairProvider {
    config: PublicationConfig,
    git: GitRepositoryProvider,
    candidates: ReviewedCandidateProvider,
    native_ci: Box<dyn RepairNativeCiPort>,
}

impl BranchFirstRepairProvider {
    pub fn new(
        config: PublicationConfig,
        git: GitRepositoryProvider,
        candidates: ReviewedCandidateProvider,
        native_ci: Box<dyn RepairNativeCiPort>,
    ) -> BranchFirstRepairProvider {
        BranchFirstRepairProvider {
            config,
            git,
            candidates,
            native_ci,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn seed(
        &mut self,
        job: &Value,
        source: &Path,
        head: &str,
        main: &str,
        directory: &Path,
        require_lease: &mut dyn FnMut() -> Result<(), ReviewError>,
    ) -> RepairResult<(PathBuf, Value)> {
        require_lease()?;
        self.git.verify(source, head)?;
        let repository = directory.join("repository");
        let intent = json!({
            "version": 1,
            "tested_candidate_commit": head,
            "current_main_commit": main,
        });
        let marker = directory.join("seed-intent.json");
        std::fs::create_dir_all(directory).map_err(|error| ReviewError::other(error.to_string()))?;
        if is_symlink(directory) || is_symlink(&repository) {
            return code("native_repair_source_changed");
        }
        if marker.exists() {
            if parse_private_json(&marker)? != intent {
                return code("native_repair_source_changed");
            }
        } else {
            write_durable(&marker, &intent)?;
        }
        let finished = directory.join("seed-result.json");
        if finished.exists() {
            let result = parse_private_json(&finished)?;
            self.git
                .verify(&repository, text_of(&result, "candidate_commit"))?;
            return Ok((repository, result));
        }
        if repository.exists() {
            // Preserve a pre-receipt crash's private clone for
            // audit. Nothing from an incomplete seed may
            // masquerade as an admitted receipt.
            let head_text = self.git.git_text(
                Some(&repository),
                &["rev-parse", "HEAD"],
                false,
                60.0,
            )?;
            let review_id = Uuid::parse_str(text_of(job, "review_id"))
                .map_err(|_| ReviewError::other("Invalid delivery identity"))?;
            let interrupted_name = format!(
                "repository.interrupted-{}",
                Uuid::new_v5(&review_id, sha(&head_text)?.as_bytes())
            );
            let interrupted = directory.join(&interrupted_name);
            if interrupted.exists() {
                return code("native_repair_source_requires_reconcile");
            }
            std::fs::rename(&repository, &interrupted)
                .map_err(|error| ReviewError::other(error.to_string()))?;
        }
        self.git.git(
            None,
            &[
                "clone",
                "--quiet",
                "--no-local",
                "--no-checkout",
                "--",
                &source.to_string_lossy(),
                &repository.to_string_lossy(),
            ],
            false,
            60.0,
        )?;
        self.git.git(
            Some(&repository),
            &["checkout", "--quiet", "--detach", head],
            false,
            60.0,
        )?;
        let remote = self.git.remote_url(text_of(job, "project_alias"));
        self.git.git(
            Some(&repository),
            &["fetch", "--quiet", "--no-tags", "--", &remote, main],
            true,
            120.0,
        )?;
        if self
            .git
            .git_text(Some(&repository), &["rev-parse", "FETCH_HEAD"], false, 60.0)?
            != main
        {
            return code("native_repair_main_changed");
        }
        require_lease()?;
        let mut conflicts: Vec<String> = Vec::new();
        if self
            .git
            .git(
                Some(&repository),
                &["merge", "--no-edit", "--no-commit", "--no-ff", "--no-gpg-sign", main],
                false,
                60.0,
            )
            .is_err()
        {
            let raw = self.git.git(
                Some(&repository),
                &["diff", "--name-only", "--diff-filter=U", "-z"],
                false,
                60.0,
            )?;
            for part in raw.split(|byte| *byte == 0) {
                if part.is_empty() {
                    continue;
                }
                let path = String::from_utf8(part.to_vec())
                    .map_err(|error| ReviewError::other(error.to_string()))?;
                conflicts.push(path);
            }
            if conflicts.is_empty() {
                return code("native_repair_merge_unavailable");
            }
        }
        if conflicts.len() > 32 || conflicts.iter().any(|path| path.len() > 1024) {
            return code("native_repair_conflicts_out_of_scope");
        }
        // Commit the honest unresolved work-in-progress, not
        // an assertion that either side won. The model
        // receives the markers and full main diff.
        self.git.git(Some(&repository), &["add", "--all", "--", "."], false, 60.0)?;
        let message = "Unverified Forge delivery repair seed; resolve integration before review";
        if self
            .git
            .git(
                Some(&repository),
                &["rev-parse", "--verify", "MERGE_HEAD"],
                false,
                60.0,
            )
            .is_err()
        {
            // Main may have raced to an ancestor already
            // present in the tested candidate. Git honestly
            // has no new merge content in that case; retain
            // the unchanged tree and explicit source parents
            // without claiming fresh gates.
            if main == head {
                return code("native_repair_identical_source");
            }
            let tree = self.git.git_text(
                Some(&repository),
                &["write-tree"],
                false,
                60.0,
            )?;
            let seed_commit = self.git.git_text(
                Some(&repository),
                &["commit-tree", &tree, "-p", head, "-p", main, "-m", message],
                false,
                60.0,
            )?;
            self.git.git(
                Some(&repository),
                &["checkout", "--quiet", "--detach", &seed_commit],
                false,
                60.0,
            )?;
        } else {
            self.git.git(
                Some(&repository),
                &["commit", "--quiet", "--no-gpg-sign", "-m", message],
                false,
                60.0,
            )?;
        }
        let candidate = sha(&self.git.git_text(
            Some(&repository),
            &["rev-parse", "HEAD"],
            false,
            60.0,
        )?)?;
        let parents_text = self.git.git_text(
            Some(&repository),
            &["show", "-s", "--format=%P", &candidate],
            false,
            60.0,
        )?;
        let parents: Vec<&str> = parents_text.split_whitespace().collect();
        if parents != [head, main] {
            return code("native_repair_parent_changed");
        }
        self.git.verify(&repository, &candidate)?;
        let tree = forge_work_worker::review_candidate::tree_map(
            &mut self.git,
            &repository,
            &candidate,
        )?;
        let source_gate = gate_source_digest(&tree, &self.config.jenkins_verification_profile);
        let canonical = crate::publication_clients::project_path(
            &self.config,
            text_of(job, "project_alias"),
        )
        .ok_or_else(|| ReviewError::other("unknown project"))?;
        let lease = forge_work_worker::workspace::WorkspaceLease {
            path: repository.clone(),
            repository: canonical,
            git_directory: repository.join(".git"),
            task_id: text_of(job, "task_id").to_string(),
            base_revision: candidate.clone(),
            source_repository: None,
            execution_branch: None,
        };
        let mut git_runner = forge_work_worker::ports_runtime::CommandGitRunner::default();
        let workspace_digest = lease
            .source_digest(&mut git_runner)
            .map_err(|error| ReviewError::other(error.to_string()))?;
        let result = json!({
            "candidate_commit": candidate,
            "integration_base": main,
            "candidate_workspace_digest": workspace_digest,
            "gate_source_sha256": source_gate,
            "gate_manifest_sha256": crate::verification::gate_manifest_sha256(
                &json!(self.config.checks),
                &json!(self.config.required_hosted_checks),
                Some(&json!(source_gate)),
            ),
            "conflict_paths": conflicts,
        });
        require_lease()?;
        write_durable(&finished, &result)?;
        Ok((repository, result))
    }

    fn details(head: &str, main: &str, build: &Value, conflicts: &Value) -> Value {
        json!({
            "version": 1,
            "tested_candidate": head,
            "current_main": main,
            "build": build,
            "conflict_paths": conflicts,
        })
    }

    fn finding(details: &Value, evidence_digest: &str) -> Value {
        let build = &details["build"];
        let conflicts = details["conflict_paths"].as_array();
        let first_path = conflicts
            .and_then(|paths| paths.first())
            .and_then(Value::as_str)
            .unwrap_or_default();
        let build_number = match &build["build_number"] {
            Value::Number(number) => number.to_string(),
            other => other.as_str().unwrap_or_default().to_string(),
        };
        let summary = format!(
            "Integrate the tested branch with current main and resolve the preserved Git conflicts. Jenkins build #{} (action {}).",
            build_number,
            build["action_id"].as_str().unwrap_or_default()
        );
        let main = text_of(details, "current_main");
        json!({
            "finding_id": format!("delivery-integration-{}", &evidence_digest[..24]),
            "category": "source",
            "check_id": "main-integration",
            "path": first_path,
            "diagnostic_code": "main_ancestor_required",
            "summary": summary,
            "reproduction": format!("Use git diff {main} HEAD and inspect conflict markers; keep all current-main changes."),
            "expected": "Resolve the source, then pass fresh Jenkins checks and independent code review.",
            "actual": forge_work_worker::diagnostic::diagnostic_text(
                &canonical_json(details),
                forge_work_worker::diagnostic::MAX_DIAGNOSTIC_BYTES,
            ),
            "evidence_digest": evidence_digest,
            "status": "open",
        })
    }

    /// Create a distinct source review after a previously
    /// approved merge fails (the merge worker's repair path).
    pub fn prepare(
        &mut self,
        job: &Value,
        current_main: &str,
        require_lease: &mut dyn FnMut() -> Result<(), ReviewError>,
    ) -> RepairResult<Value> {
        let main = sha(current_main)?;
        let head = sha(text_of(job, "candidate_commit"))?;
        let original = self.candidates.read(text_of(job, "review_id"))?;
        let mut acquire_job = job.clone();
        acquire_job["attempt_id"] = original["attempt_id"].clone();
        acquire_job["workspace_digest"] = original["workspace_digest"].clone();
        let (source, _) = self.candidates.acquire(&acquire_job)?;
        let build = self.native_ci.build_reference(job)?;
        let merge_id = Uuid::parse_str(text_of(job, "merge_id"))
            .map_err(|_| ReviewError::other("Invalid delivery identity"))?;
        let review_id = Uuid::new_v5(&merge_id, "branch-first-repair-source-v1".as_bytes()).to_string();
        let mut selected = job.clone();
        selected["review_id"] = json!(review_id);
        selected["attempt_id"] = original["attempt_id"].clone();
        let directory = self.candidates.directory(&review_id)?;
        let (_, result) = self.seed(&selected, &source, &head, &main, &directory, require_lease)?;
        let details = Self::details(&head, &main, &build, &result["conflict_paths"]);
        let evidence = sha256_hex(
            canonical_json(&json!({
                "source_review_id": text_of(job, "review_id"),
                "merge_id": text_of(job, "merge_id"),
                "seed": result,
                "details": details,
            }))
            .as_bytes(),
        );
        let finding = Self::finding(&details, &evidence);
        let mut manifest = original.clone();
        if let Some(map) = manifest.as_object_mut() {
            map.insert("review_id".into(), json!(review_id));
            map.insert("candidate_commit".into(), result["candidate_commit"].clone());
            map.insert(
                "candidate_workspace_digest".into(),
                result["candidate_workspace_digest"].clone(),
            );
            map.insert("integration_base".into(), json!(main));
            map.insert("repair_requires_new_workspace".into(), json!(true));
            map.insert("checks_passed".into(), json!(false));
            map.insert("gate_manifest_sha256".into(), result["gate_manifest_sha256"].clone());
            map.insert("gate_source_sha256".into(), result["gate_source_sha256"].clone());
            map.insert("delivery_failure".into(), details.clone());
            map.remove("native_ci");
        }
        self.candidates.record(&selected, &manifest)?;
        require_lease()?;
        Ok(json!({
            "version": 1,
            "reason": "merge_main_requires_reverification",
            "summary": finding["summary"],
            "source_review_id": review_id,
            "candidate_commit": result["candidate_commit"],
            "candidate_workspace_digest": result["candidate_workspace_digest"],
            "integration_base": main,
            "gate_manifest_sha256": result["gate_manifest_sha256"],
            "gate_source_sha256": result["gate_source_sha256"],
            "tested_candidate_commit": head,
            "evidence_digest": evidence,
            "findings": [finding],
            "build": build,
            "delivery_failure": details,
            "no_merge_confirmed": true,
        }))
    }
}

/// The merge worker's repair surface over the same provider
/// (the factory passes one repair provider to both workers).
impl crate::merge::MergeRepairPort for BranchFirstRepairProvider {
    fn prepare(
        &mut self,
        job: &Value,
        current_main: &str,
        require_lease: crate::delivery_stores::RequireLease,
    ) -> Result<Value, ProviderError> {
        let mut lease = || {
            require_lease().map_err(|error| ReviewError::other(error.code().to_string()))
        };
        BranchFirstRepairProvider::prepare(self, job, current_main, &mut lease)
            .map_err(|error| ProviderError::Other(error.to_string()))
    }
}

impl RepairSourcesPort for BranchFirstRepairProvider {
    fn prepare_local(
        &mut self,
        job: &Value,
        repository: &Path,
        native_binding: Option<&Value>,
        build: &Value,
        require_lease: &mut dyn FnMut() -> Result<(), ReviewError>,
    ) -> Result<Value, ReviewError> {
        // The failed local review has never approved either
        // tree.
        let head = sha(text_of(job, "candidate_commit"))?;
        let main = sha(text_of(job, "integration_base"))?;
        let directory = self.candidates.directory(text_of(job, "review_id"))?;
        let seed_directory = directory.join("delivery-repair");
        let destination = directory.join("repository");
        let tested = directory.join("repository.tested");
        let install = directory.join("local-seed-install.json");
        let (seed, result) = if install.exists() {
            (
                seed_directory.join("repository"),
                parse_private_json(&seed_directory.join("seed-result.json"))?,
            )
        } else {
            self.seed(job, repository, &head, &main, &seed_directory, require_lease)?
        };
        let intent = json!({
            "version": 1,
            "tested_candidate": head,
            "candidate_commit": result["candidate_commit"],
            "current_main": main,
        });
        if install.exists() {
            if parse_private_json(&install)? != intent {
                return code("native_repair_tested_source_changed");
            }
        } else {
            write_durable(&install, &intent)?;
        }
        let details = Self::details(&head, &main, build, &result["conflict_paths"]);
        let binding = native_binding.cloned().unwrap_or(Value::Null);
        let evidence = sha256_hex(
            canonical_json(&json!({
                "binding": binding,
                "seed": result,
                "details": details,
            }))
            .as_bytes(),
        );
        require_lease()?;
        if is_symlink(&tested) || is_symlink(&destination) || is_symlink(&seed) {
            return code("native_repair_tested_source_changed");
        }
        if !tested.exists() {
            self.git.verify(&destination, &head)?;
            std::fs::rename(&destination, &tested)
                .map_err(|error| ReviewError::other(error.to_string()))?;
        }
        self.git.verify(&tested, &head)?;
        if !destination.exists() {
            self.git
                .verify(&seed, text_of(&result, "candidate_commit"))?;
            std::fs::rename(&seed, &destination)
                .map_err(|error| ReviewError::other(error.to_string()))?;
        }
        self.git
            .verify(&destination, text_of(&result, "candidate_commit"))?;
        let finding = Self::finding(&details, &evidence);
        let mut out = result.clone();
        if let Some(map) = out.as_object_mut() {
            map.insert(
                "repository".into(),
                json!(destination.to_string_lossy().into_owned()),
            );
            map.insert("delivery_failure".into(), details);
            map.insert("evidence_digest".into(), json!(evidence));
            map.insert("findings".into(), json!([finding]));
        }
        Ok(out)
    }
}
