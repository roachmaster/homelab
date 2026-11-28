//! The release state machine (PortalReleaseActor in
//! portal_release_actor.py; its ActorFactory is in `crate::forge`).
//! State, plans and jobs are JSON values, exactly as the durable
//! state.json holds them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::{
    artifact_commands, canonical, decode, is_gate_ref, is_sha40, is_sha64, main_checks,
    safe_relative, sha, v3_main_checks, v3_main_phases, ActorConfig, CORE, IDENTITY, POLICY,
    STARTUP_IDENTITY_SECONDS, UNIT,
};
use crate::forge::ActorCollaborators;
use crate::ports::{ActorClock, ProbeProvider, RootFiles};
use crate::system::SystemPort;

struct ReceiptEvidence {
    health: bool,
    rollback: bool,
    entry: bool,
}

pub fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
        })
}

fn get_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn map_of(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

pub struct PortalReleaseActor {
    pub config: ActorConfig,
    system: Box<dyn SystemPort>,
    files: Box<dyn RootFiles>,
    probes: Box<dyn ProbeProvider>,
    clock: Box<dyn ActorClock>,
    /// The lease and capability of the invocation in progress; set by
    /// `invoke`, not a collaborator.
    authority: Option<Value>,
}

impl PortalReleaseActor {
    /// Bind the actor to its configuration and collaborators.
    pub fn new(config: ActorConfig, collaborators: ActorCollaborators) -> PortalReleaseActor {
        PortalReleaseActor {
            config,
            system: collaborators.system,
            files: collaborators.files,
            probes: collaborators.probes,
            clock: collaborators.clock,
            authority: None,
        }
    }

    fn authorize(&self, job: &Value, start: bool, plan_sha256: Option<&str>) -> Result<(), String> {
        let authority = self
            .authority
            .clone()
            .ok_or_else(|| "Release lease or capability is no longer current".to_string())?;
        // The authorizer is the system port by default (Python:
        // authorizer or system.authorize).
        if !self.system.authorize(job, &authority, start, plan_sha256)? {
            return Err("Release lease or capability is no longer current".to_string());
        }
        Ok(())
    }

    fn job(&self, job: &Value) -> Result<Map<String, Value>, String> {
        let object = job
            .as_object()
            .ok_or_else(|| "Unexpected release identity fields".to_string())?;
        if object.len() != IDENTITY.len() || !IDENTITY.iter().all(|name| object.contains_key(*name))
        {
            return Err("Unexpected release identity fields".to_string());
        }
        if get_str(job, "owner_id") != Some(self.config.owner.as_str())
            || get_str(job, "project_alias") != Some(self.config.project_alias.as_str())
        {
            return Err("Release scope denied".to_string());
        }
        for name in ["release_id", "verification_id", "ci_action_id"] {
            let value = get_str(job, name).ok_or("Invalid release UUID")?;
            if !is_canonical_uuid(value) {
                return Err("Invalid release UUID".to_string());
            }
        }
        for (name, check) in [
            ("commit_sha", is_sha40 as fn(&str) -> bool),
            ("tree_sha", is_sha40),
            ("ci_receipt_sha256", is_sha64),
            ("gate_manifest_sha256", is_gate_ref),
        ] {
            let value = get_str(job, name).ok_or("Invalid release source digest")?;
            if !check(value) {
                return Err("Invalid release source digest".to_string());
            }
        }
        Ok(object.clone())
    }

    fn paths(&self, job: &Value) -> (PathBuf, PathBuf) {
        let release_id = get_str(job, "release_id").unwrap_or("");
        let commit = get_str(job, "commit_sha").unwrap_or("");
        let state = self.config.state_root.join(release_id);
        let release = self
            .config
            .releases_root
            .join(format!("portal-auto-{commit}-{release_id}"));
        (state, release)
    }

    fn save(&self, directory: &Path, state: &Map<String, Value>) -> Result<(), String> {
        let mut raw = canonical(&Value::Object(state.clone()));
        raw.push(b'\n');
        self.files
            .write(&directory.join("state.json"), &raw, 0o600, 0)
    }

    fn diagnostic(
        &self,
        directory: &Path,
        state: &mut Map<String, Value>,
        stage: &str,
    ) -> Result<(), String> {
        const STAGES: [&str; 12] = [
            "activation_intent",
            "service_reload",
            "service_restart",
            "startup_identity_pending",
            "startup_identity_timeout",
            "startup_identity_read_failed",
            "startup_command_mismatch",
            "startup_config_mismatch",
            "startup_process_mismatch",
            "live_health",
            "live_health_failed",
            "activation_call_failed",
        ];
        if !STAGES.contains(&stage) {
            return Err("Unknown private activation diagnostic stage".to_string());
        }
        state.insert(
            "activation_diagnostic_stage".to_string(),
            Value::String(stage.to_string()),
        );
        self.save(directory, state)
    }

    fn current(&self) -> Result<Map<String, Value>, String> {
        let mut current = self.system.current()?;
        let binary = get_str(&Value::Object(current.clone()), "binary")
            .unwrap_or("")
            .to_string();
        let config_path = get_str(&Value::Object(current.clone()), "config")
            .unwrap_or("")
            .to_string();
        let binary_path = PathBuf::from(&binary);
        let config = PathBuf::from(&config_path);
        let inside_releases = binary_path.strip_prefix(&self.config.releases_root).is_ok();
        if !inside_releases || config.parent() != Some(self.config.web_config_root.as_path()) {
            return Err("Portal command escaped configured release boundary".to_string());
        }
        let raw = self.files.read(&config, 0)?;
        let value = decode(&raw)?;
        if get_str(&value, "bind") != Some("127.0.0.1:8788")
            || get_str(&value, "public_origin") != Some("https://portal.thedevex.com")
            || get_str(&value, "session_auth") != Some("trusted_proxy")
        {
            return Err("Portal admission configuration changed".to_string());
        }
        let override_raw = self.files.read(&self.config.override_path, 0)?;
        current.insert("config_sha256".to_string(), Value::String(sha(&raw)));
        current.insert(
            "override".to_string(),
            Value::String(String::from_utf8_lossy(&override_raw).into_owned()),
        );
        current.insert(
            "override_sha256".to_string(),
            Value::String(sha(&override_raw)),
        );
        current.insert("value".to_string(), value);
        Ok(current)
    }

    fn proof(&self, job: &Value) -> Result<(PathBuf, String, Map<String, Value>), String> {
        let ci_action = get_str(job, "ci_action_id").unwrap_or("");
        let raw = self.files.read(
            &self
                .config
                .ci_results_root
                .join(format!("{ci_action}.json")),
            0,
        )?;
        let result = decode(&raw)?;
        if sha(&raw) != get_str(job, "ci_receipt_sha256").unwrap_or("") {
            return Err("CI result digest mismatch".to_string());
        }
        let profile = get_str(&result, "verification_profile").unwrap_or("");
        if !matches!(profile, "forge-sdlc-jenkins-v2" | "forge-sdlc-jenkins-v3") {
            return Err("Unsupported full main proof".to_string());
        }
        let version: i64 = if profile == "forge-sdlc-jenkins-v3" {
            3
        } else {
            2
        };
        let commands = if version == 3 {
            v3_main_checks()
        } else {
            main_checks()
        };
        if version == 3 {
            let phases = v3_main_phases();
            let kinds: BTreeSet<String> = phases
                .iter()
                .filter_map(|row| row.get("kind").and_then(Value::as_str).map(str::to_string))
                .collect();
            let expected_kinds: Vec<Value> = kinds
                .iter()
                .map(|kind| Value::String(kind.clone()))
                .collect();
            if result.get("phase_results") != Some(&Value::Array(phases))
                || result.get("failure_kind") != Some(&Value::Null)
                || result.get("passed_kinds") != Some(&Value::Array(expected_kinds))
            {
                return Err("Release requires every exact protected v3 phase".to_string());
            }
        }
        let expected: [(&str, Value); 8] = [
            ("action_id", Value::String(ci_action.to_string())),
            (
                "candidate_commit",
                Value::String(get_str(job, "commit_sha").unwrap_or("").to_string()),
            ),
            (
                "candidate_tree",
                Value::String(get_str(job, "tree_sha").unwrap_or("").to_string()),
            ),
            (
                "expected_main",
                Value::String(get_str(job, "commit_sha").unwrap_or("").to_string()),
            ),
            (
                "gate_manifest_sha256",
                Value::String(
                    get_str(job, "gate_manifest_sha256")
                        .unwrap_or("")
                        .to_string(),
                ),
            ),
            ("branch_scope", Value::String("main".to_string())),
            ("verification_profile", Value::String(profile.to_string())),
            ("status", Value::String("passed".to_string())),
        ];
        let checks_expected: Vec<Value> = commands
            .iter()
            .enumerate()
            .map(|(index, argv)| {
                json!({
                    "index": index,
                    "argv": argv,
                    "returncode": 0,
                })
            })
            .collect();
        let checks_ok = result
            .get("checks")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter().all(|row| {
                    row.get("index").map(Value::is_i64).unwrap_or(false)
                        && row.get("returncode").map(Value::is_i64).unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if expected
            .iter()
            .any(|(key, value)| result.get(*key) != Some(value))
            || result.get("version") != Some(&json!(version))
            || !result.get("version").map(Value::is_i64).unwrap_or(false)
            || result.get("cleanup_confirmed") != Some(&Value::Bool(true))
            || result.get("checks") != Some(&Value::Array(checks_expected))
            || !checks_ok
            || result.get("coverage")
                != Some(&json!({
                    "scope": "main",
                    "browser": "required",
                    "database": "required_zero_skips",
                    "backend_suites": "all_four",
                }))
        {
            return Err("Release requires exact full main verification and cleanup".to_string());
        }
        let export = result
            .get("artifact_export")
            .ok_or_else(|| "Main verification has no trusted deployment artifacts".to_string())?;
        if export.get("version") != Some(&json!(1))
            || get_str(export, "policy") != Some("forge-portal-artifact-v1")
            || get_str(export, "action_id") != Some(ci_action)
            || get_str(export, "source_commit") != get_str(job, "commit_sha")
            || get_str(export, "source_tree") != get_str(job, "tree_sha")
        {
            return Err("Main verification has no trusted deployment artifacts".to_string());
        }
        let root = self
            .config
            .ci_results_root
            .join(format!("{ci_action}.artifacts"));
        let manifest_raw = self.files.read(&root.join("manifest.json"), 0)?;
        if sha(&manifest_raw) != get_str(export, "manifest_sha256").unwrap_or("") {
            return Err("Artifact manifest digest mismatch".to_string());
        }
        let manifest = decode(&manifest_raw)?;
        let manifest_keys: BTreeSet<&String> = manifest
            .as_object()
            .map(|object| object.keys().collect())
            .unwrap_or_default();
        if manifest_keys.len() != 8
            || manifest.get("version") != Some(&json!(1))
            || get_str(&manifest, "policy") != Some("forge-portal-artifact-v1")
            || get_str(&manifest, "action_id") != Some(ci_action)
            || get_str(&manifest, "source_commit") != get_str(job, "commit_sha")
            || get_str(&manifest, "source_tree") != get_str(job, "tree_sha")
            || manifest.get("commands") != Some(&Value::Array(artifact_commands()))
            || manifest.get("request_sha256") != result.get("request_sha256")
        {
            return Err("Artifact provenance mismatch".to_string());
        }
        let files = manifest
            .get("files")
            .and_then(Value::as_array)
            .filter(|files| (7..=4096).contains(&files.len()))
            .ok_or_else(|| "Invalid artifact inventory".to_string())?;
        let mut inventory = Map::new();
        let mut total: i64 = 0;
        for item in files {
            if !item.is_object() {
                return Err("Invalid artifact entry".to_string());
            }
            let name = safe_relative(
                item.get("path")
                    .ok_or_else(|| "Invalid artifact entry".to_string())?,
            )?;
            let keys: BTreeSet<&String> = item.as_object().unwrap().keys().collect();
            let executable_expected = name == "bin/forge-ide-web-host";
            if keys.len() != 4
                || !item.get("byte_count").map(Value::is_i64).unwrap_or(false)
                || !item
                    .get("executable")
                    .map(Value::is_boolean)
                    .unwrap_or(false)
                || item.get("executable") != Some(&Value::Bool(executable_expected))
                || inventory.contains_key(&name)
                || !(executable_expected || name.starts_with("assets/"))
            {
                return Err("Artifact exceeds portal-only boundary".to_string());
            }
            let raw = self.files.read(&root.join(&name), 128 * 1024 * 1024)?;
            if sha(&raw) != item.get("sha256").and_then(Value::as_str).unwrap_or("")
                || raw.len() as i64 != item.get("byte_count").and_then(Value::as_i64).unwrap_or(-1)
            {
                return Err("Artifact file digest mismatch".to_string());
            }
            total += raw.len() as i64;
            if total > 256 * 1024 * 1024 {
                return Err("Portal packet exceeds bound".to_string());
            }
            inventory.insert(
                name,
                json!({
                    "sha256": sha(&raw),
                    "byte_count": raw.len(),
                    "executable": executable_expected,
                }),
            );
        }
        if export.get("file_count") != Some(&json!(inventory.len()))
            || export.get("total_bytes") != Some(&json!(total))
        {
            return Err("Artifact inventory accounting mismatch".to_string());
        }
        let mut required: Vec<String> = vec![
            "bin/forge-ide-web-host".to_string(),
            "assets/manifest.json".to_string(),
        ];
        required.extend(CORE.iter().map(|name| format!("assets/{name}")));
        if required.iter().any(|name| !inventory.contains_key(name)) {
            return Err("Incomplete portal packet".to_string());
        }
        let assets = decode(&self.files.read(&root.join("assets/manifest.json"), 0)?)?;
        if assets.get("format_version") != Some(&json!(1))
            || assets.get("abi_version") != Some(&json!(self.config.expected_abi))
        {
            return Err("Portal asset ABI mismatch".to_string());
        }
        for item in assets
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let relative = safe_relative(
                item.get("path")
                    .ok_or_else(|| "Portal manifest file mismatch".to_string())?,
            )?;
            let name = format!("assets/{relative}");
            if inventory.get(&name).and_then(|entry| entry.get("sha256")) != item.get("sha256") {
                return Err("Portal manifest file mismatch".to_string());
            }
        }
        Ok((
            root,
            get_str(export, "manifest_sha256").unwrap_or("").to_string(),
            inventory,
        ))
    }
}

impl PortalReleaseActor {
    fn public_parent(&self, root: &Path, directory: &Path) -> Result<(), String> {
        self.files.public_parent(root, directory)
    }

    fn archive(&self, current: &Map<String, Value>) -> Result<(), String> {
        let assets = current
            .get("value")
            .and_then(|value| get_str(value, "assets"))
            .ok_or("Old replay source no longer matches its manifest")?
            .to_string();
        let root = PathBuf::from(&assets).join("game-assets");
        let manifest_raw = self.files.read(&root.join("manifest.json"), 0)?;
        let manifest = decode(&manifest_raw)?;
        let digest = sha(&manifest_raw);
        let old = self.config.replay_archive_root.join(&digest);
        for name in [
            "manifest.json",
            "levels/room-one.json",
            "levels/traversal-yard.json",
        ] {
            let raw = if name == "manifest.json" {
                manifest_raw.clone()
            } else {
                let raw = self.files.read(&root.join(name), 0)?;
                let key = if name.ends_with("room-one.json") {
                    "room-one"
                } else {
                    "traversal-yard"
                };
                let entry = manifest
                    .get("entries")
                    .and_then(|entries| entries.get(key))
                    .ok_or("Old replay source no longer matches its manifest")?;
                if get_str(entry, "path") != Some(name)
                    || entry.get("sha256").and_then(Value::as_str) != Some(sha(&raw).as_str())
                {
                    return Err("Old replay source no longer matches its manifest".to_string());
                }
                raw
            };
            let target = old.join(name);
            self.public_parent(&self.config.replay_archive_root, target.parent().unwrap())?;
            if target.exists() {
                if self.files.read(&target, 0)? != raw {
                    return Err("Retained replay archive conflict".to_string());
                }
            } else {
                self.files.write(&target, &raw, 0o444, 0)?;
            }
        }
        Ok(())
    }

    pub fn plan(&mut self, job: &Value) -> Result<Map<String, Value>, String> {
        let job = Value::Object(self.job(job)?);
        let (directory, release) = self.paths(&job);
        let path = directory.join("state.json");
        if path.exists() {
            let state = map_of(&decode(&self.files.read(&path, 0)?)?);
            if state.get("job") != Some(&job) {
                return Err("Release identity replay conflict".to_string());
            }
            if let Some(outcome) = state.get("outcome") {
                return Ok(map_of(outcome));
            }
            if matches!(
                get_str(&Value::Object(state.clone()), "phase"),
                Some("staging" | "candidate")
            ) {
                let plan = state.get("plan").cloned().unwrap_or(Value::Null);
                return self.reconcile(&job, &plan);
            }
            return Ok(json!({"status": "planned", "plan": state.get("plan").cloned().unwrap_or(Value::Null)})
                .as_object()
                .unwrap()
                .clone());
        }
        let (packet, manifest_sha, inventory) = self.proof(&job)?;
        let current = self.current()?;
        let mut public = Map::new();
        public.insert("version".to_string(), json!(1));
        public.insert("policy".to_string(), Value::String(POLICY.to_string()));
        for (key, value) in job.as_object().unwrap() {
            public.insert(key.clone(), value.clone());
        }
        public.insert(
            "artifact_manifest_sha256".to_string(),
            Value::String(manifest_sha),
        );
        let plan_sha = sha(&canonical(&Value::Object(public.clone())));
        public.insert("plan_sha256".to_string(), Value::String(plan_sha));
        let mut state = Map::new();
        state.insert("job".to_string(), job.clone());
        state.insert("plan".to_string(), Value::Object(public.clone()));
        state.insert("phase".to_string(), Value::String("staging".to_string()));
        state.insert("before".to_string(), Value::Object(current));
        state.insert(
            "release".to_string(),
            Value::String(release.to_string_lossy().into_owned()),
        );
        state.insert("inventory".to_string(), Value::Object(inventory));
        state.insert("restart_requested".to_string(), json!(false));
        state.insert("rollback_restart_requested".to_string(), json!(false));
        state.insert("rollback_restart_attempts".to_string(), json!(0));
        self.save(&directory, &state)?;
        if self.system.main_head(&self.config)? != get_str(&job, "commit_sha").unwrap_or("") {
            return self.receipt(
                &directory,
                &mut state,
                "superseded",
                ReceiptEvidence {
                    health: false,
                    rollback: false,
                    entry: false,
                },
                "Main advanced before release preparation; portal was not changed.",
            );
        }
        self.finish_stage(&directory, &mut state, &packet)
    }

    fn finish_stage(
        &mut self,
        directory: &Path,
        state: &mut Map<String, Value>,
        packet: &Path,
    ) -> Result<Map<String, Value>, String> {
        use std::os::unix::fs::PermissionsExt;
        let release =
            PathBuf::from(get_str(&Value::Object(state.clone()), "release").unwrap_or(""));
        let web_gid = nix::unistd::Group::from_name(&self.config.web_group)
            .ok()
            .flatten()
            .map(|group| group.gid.as_raw())
            .ok_or_else(|| "Unknown web group".to_string())?;
        std::fs::create_dir_all(&release).map_err(|error| error.to_string())?;
        std::fs::set_permissions(&release, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        let inventory = map_of(state.get("inventory").unwrap_or(&Value::Null));
        for (name, expected) in &inventory {
            let raw = self.files.read(&packet.join(name), 128 * 1024 * 1024)?;
            if sha(&raw) != expected.get("sha256").and_then(Value::as_str).unwrap_or("") {
                return Err("Sealed packet changed".to_string());
            }
            let target = release.join(name);
            self.public_parent(&release, target.parent().unwrap())?;
            if target.exists() {
                if self.files.read(&target, 128 * 1024 * 1024)? != raw {
                    return Err("Release staging conflict".to_string());
                }
            } else {
                let mode = if expected.get("executable") == Some(&Value::Bool(true)) {
                    0o555
                } else {
                    0o444
                };
                self.files.write(&target, &raw, mode, 0)?;
            }
        }
        let mut directories: Vec<PathBuf> = Vec::new();
        collect_directories(&release, &mut directories);
        directories.sort_by(|a, b| b.cmp(a));
        for path in &directories {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
                .map_err(|error| error.to_string())?;
        }
        std::fs::set_permissions(&release, std::fs::Permissions::from_mode(0o555))
            .map_err(|error| error.to_string())?;
        let before = map_of(state.get("before").unwrap_or(&Value::Null));
        self.archive(&before)?;
        let mut config = map_of(before.get("value").unwrap_or(&Value::Null));
        if let Some(presence) = &self.config.mcp_presence {
            if !config.get("work").map(Value::is_object).unwrap_or(false) {
                return Err("MCP presence requires the existing scoped Work relay".to_string());
            }
            let mut work = map_of(config.get("work").unwrap());
            work.insert("mcp_presence".to_string(), Value::Object(presence.clone()));
            config.insert("work".to_string(), Value::Object(work));
        }
        config.insert(
            "assets".to_string(),
            Value::String(release.join("assets").to_string_lossy().into_owned()),
        );
        config.insert(
            "cooked_replay_archive".to_string(),
            Value::String(
                self.config
                    .replay_archive_root
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        let job = state.get("job").cloned().unwrap_or(Value::Null);
        let release_id = get_str(&job, "release_id").unwrap_or("");
        let mut live_raw = canonical(&Value::Object(config.clone()));
        live_raw.push(b'\n');
        let live_path = self
            .config
            .web_config_root
            .join(format!("automatic-{release_id}.json"));
        self.files.write(&live_path, &live_raw, 0o640, web_gid)?;
        let mut candidate = config.clone();
        candidate.insert(
            "bind".to_string(),
            Value::String(format!("127.0.0.1:{}", self.config.candidate_port)),
        );
        let mut candidate_raw = canonical(&Value::Object(candidate.clone()));
        candidate_raw.push(b'\n');
        let candidate_path = self
            .config
            .web_config_root
            .join(format!("candidate-{release_id}.json"));
        self.files
            .write(&candidate_path, &candidate_raw, 0o640, web_gid)?;
        state.insert(
            "live_config".to_string(),
            Value::String(live_path.to_string_lossy().into_owned()),
        );
        state.insert(
            "live_config_sha256".to_string(),
            Value::String(sha(&live_raw)),
        );
        state.insert("phase".to_string(), Value::String("candidate".to_string()));
        self.save(directory, state)?;
        let probe_failed = (|| -> Result<(), String> {
            self.system.candidate_start(
                &self.config,
                release_id,
                &release.join("bin/forge-ide-web-host"),
                &candidate_path,
            )?;
            let inventory = state.get("inventory").cloned().unwrap_or(Value::Null);
            self.health(&candidate, &inventory)
        })()
        .is_err();
        let _ = self.system.candidate_stop(release_id);
        if probe_failed {
            let current = self.current()?;
            let before = map_of(state.get("before").unwrap_or(&Value::Null));
            if current.get("binary") != before.get("binary")
                || current.get("override_sha256") != before.get("override_sha256")
                || current.get("config_sha256") != before.get("config_sha256")
            {
                return self.receipt(
                    directory,
                    state,
                    "failed",
                    ReceiptEvidence {
                        health: false,
                        rollback: false,
                        entry: false,
                    },
                    "Candidate failed; an independent portal change was preserved.",
                );
            }
            let old_assets = PathBuf::from(
                before
                    .get("value")
                    .and_then(|value| get_str(value, "assets"))
                    .unwrap_or(""),
            );
            let mut old_inventory = Map::new();
            for name in CORE {
                let raw = self.files.read(&old_assets.join(name), 8 * 1024 * 1024)?;
                old_inventory.insert(format!("assets/{name}"), json!({"sha256": sha(&raw)}));
            }
            let before_value = before.get("value").cloned().unwrap_or(Value::Null);
            self.live_health(&map_of(&before_value), &Value::Object(old_inventory))?;
            return self.receipt(
                directory,
                state,
                "failed",
                ReceiptEvidence { health: true, rollback: true, entry: true },
                "Candidate health failed; the existing healthy portal was retained without a restart.",
            );
        }
        state.insert("phase".to_string(), Value::String("prepared".to_string()));
        self.save(directory, state)?;
        Ok(
            json!({"status": "planned", "plan": state.get("plan").cloned().unwrap_or(Value::Null)})
                .as_object()
                .unwrap()
                .clone(),
        )
    }

    fn health(&self, config: &Map<String, Value>, inventory: &Value) -> Result<(), String> {
        let bind = get_str(&Value::Object(config.clone()), "bind")
            .unwrap_or("")
            .to_string();
        let port: u32 = bind
            .split(':')
            .next_back()
            .and_then(|part| part.parse().ok())
            .ok_or("Portal health verification failed")?;
        let origin = get_str(&Value::Object(config.clone()), "public_origin")
            .unwrap_or("")
            .to_string();
        let deadline = self.clock.monotonic() + 60.0;
        loop {
            let mut probe = self.probes.probe(&origin, port);
            match probe.check(inventory, &self.config.project_alias) {
                Ok(_) => return Ok(()),
                Err(_) => {
                    if self.clock.monotonic() >= deadline {
                        return Err("Portal health verification failed".to_string());
                    }
                    self.clock.sleep(0.5);
                }
            }
        }
    }

    fn live_health(&self, config: &Map<String, Value>, inventory: &Value) -> Result<(), String> {
        self.health(config, inventory)?;
        self.system.ensure_entry_bridge()?;
        let mut entry = config.clone();
        entry.insert(
            "bind".to_string(),
            Value::String(format!("127.0.0.1:{}", crate::config::ENTRY_PORT)),
        );
        self.health(&entry, inventory)
    }

    fn load(&self, job: &Value, public: &Value) -> Result<(PathBuf, Map<String, Value>), String> {
        let (directory, _) = self.paths(job);
        let state = map_of(&decode(
            &self.files.read(&directory.join("state.json"), 0)?,
        )?);
        if state.get("job") != Some(job)
            || canonical(public) != canonical(state.get("plan").unwrap_or(&Value::Null))
        {
            return Err("Release plan replay conflict".to_string());
        }
        Ok((directory, state))
    }

    fn receipt(
        &self,
        directory: &Path,
        state: &mut Map<String, Value>,
        status: &str,
        evidence: ReceiptEvidence,
        summary: &str,
    ) -> Result<Map<String, Value>, String> {
        let ReceiptEvidence {
            health,
            rollback,
            entry,
        } = evidence;
        let plan = map_of(state.get("plan").unwrap_or(&Value::Null));
        let mut body = Map::new();
        body.insert("version".to_string(), json!(1));
        body.insert("policy".to_string(), Value::String(POLICY.to_string()));
        body.insert("status".to_string(), Value::String(status.to_string()));
        body.insert(
            "job".to_string(),
            state.get("job").cloned().unwrap_or(Value::Null),
        );
        body.insert(
            "plan_sha256".to_string(),
            plan.get("plan_sha256").cloned().unwrap_or(Value::Null),
        );
        body.insert("health_confirmed".to_string(), json!(health));
        body.insert("cleanup_confirmed".to_string(), json!(true));
        body.insert("rollback_confirmed".to_string(), json!(rollback));
        body.insert("entry_bridge_confirmed".to_string(), json!(entry));
        body.insert("unit".to_string(), Value::String(UNIT.to_string()));
        let mut raw = canonical(&Value::Object(body));
        raw.push(b'\n');
        self.files
            .write(&directory.join("activation.json"), &raw, 0o600, 0)?;
        let mut receipt = plan.clone();
        receipt.insert("unit".to_string(), Value::String(UNIT.to_string()));
        receipt.insert("health_confirmed".to_string(), json!(health));
        receipt.insert("cleanup_confirmed".to_string(), json!(true));
        receipt.insert("rollback_confirmed".to_string(), json!(rollback));
        receipt.insert("entry_bridge_confirmed".to_string(), json!(entry));
        receipt.insert(
            "activation_receipt_sha256".to_string(),
            Value::String(sha(&raw)),
        );
        let outcome = json!({
            "status": status,
            "receipt": receipt,
            "summary": summary,
        });
        state.insert("outcome".to_string(), outcome.clone());
        state.insert("phase".to_string(), Value::String(status.to_string()));
        self.save(directory, state)?;
        Ok(map_of(&outcome))
    }
}

fn collect_directories(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !path.is_symlink() {
                out.push(path.clone());
                collect_directories(&path, out);
            }
        }
    }
}

impl PortalReleaseActor {
    fn observe_target(
        &mut self,
        directory: &Path,
        state: &mut Map<String, Value>,
    ) -> Option<Map<String, Value>> {
        let started = state.get("startup_started_at").and_then(Value::as_f64);
        let deadline = state.get("startup_deadline_at").and_then(Value::as_f64);
        let now = self.clock.wall();
        let valid_window = match (started, deadline) {
            (Some(started), Some(deadline)) => {
                started > 0.0
                    && started <= deadline
                    && deadline - started <= STARTUP_IDENTITY_SECONDS
                    && started <= now
                    && now < deadline
            }
            _ => false,
        };
        let local_deadline = self.clock.monotonic()
            + if valid_window {
                deadline.unwrap() - now
            } else {
                0.0
            };
        let target = PathBuf::from(get_str(&Value::Object(state.clone()), "release").unwrap_or(""))
            .join("bin/forge-ide-web-host")
            .to_string_lossy()
            .into_owned();
        loop {
            let current = match self.current() {
                Ok(current) => current,
                Err(_) => {
                    let _ = self.diagnostic(directory, state, "startup_identity_read_failed");
                    return None;
                }
            };
            if current.get("binary").and_then(Value::as_str) != Some(target.as_str())
                || current.get("config").and_then(Value::as_str)
                    != get_str(&Value::Object(state.clone()), "live_config")
            {
                let _ = self.diagnostic(directory, state, "startup_command_mismatch");
                return None;
            }
            if current.get("override_sha256") != state.get("target_override_sha256")
                || current.get("config_sha256") != state.get("live_config_sha256")
            {
                let _ = self.diagnostic(directory, state, "startup_config_mismatch");
                return None;
            }
            if current.get("process_binary").and_then(Value::as_str) == Some(target.as_str()) {
                return Some(current);
            }
            let process_binary = current
                .get("process_binary")
                .cloned()
                .unwrap_or(Value::Null);
            let transient = current.get("service_type").and_then(Value::as_str) == Some("simple")
                && matches!(
                    current.get("active_state").and_then(Value::as_str),
                    Some("active" | "activating")
                )
                && matches!(
                    current.get("sub_state").and_then(Value::as_str),
                    Some("running" | "start" | "start-pre" | "start-post")
                )
                && (matches!(
                    process_binary.as_str(),
                    Some("/usr/lib/systemd/systemd-executor" | "/lib/systemd/systemd-executor")
                ) || (process_binary.is_null()
                    && current.get("startup_job") == Some(&Value::Bool(true))));
            if !transient {
                let _ = self.diagnostic(directory, state, "startup_process_mismatch");
                return None;
            }
            let now = self.clock.wall();
            if !valid_window
                || now < started.unwrap_or(0.0)
                || now >= deadline.unwrap_or(0.0)
                || self.clock.monotonic() >= local_deadline
            {
                let _ = self.diagnostic(directory, state, "startup_identity_timeout");
                return None;
            }
            if state
                .get("activation_diagnostic_stage")
                .and_then(Value::as_str)
                != Some("startup_identity_pending")
            {
                let _ = self.diagnostic(directory, state, "startup_identity_pending");
            }
            let wait = (deadline.unwrap() - now)
                .min(local_deadline - self.clock.monotonic())
                .min(0.05);
            self.clock.sleep(wait.max(0.0));
        }
    }

    pub fn activate(&mut self, job: &Value, public: &Value) -> Result<Map<String, Value>, String> {
        let (directory, mut state) = self.load(job, public)?;
        if let Some(outcome) = state.get("outcome") {
            return Ok(map_of(outcome));
        }
        if get_str(&Value::Object(state.clone()), "phase") != Some("prepared") {
            return self.reconcile(job, public);
        }
        if self.system.main_head(&self.config)? != get_str(job, "commit_sha").unwrap_or("") {
            return self.receipt(
                &directory,
                &mut state,
                "superseded",
                ReceiptEvidence {
                    health: false,
                    rollback: false,
                    entry: false,
                },
                "Main advanced; the previous release was retained.",
            );
        }
        let current = self.current()?;
        let before = map_of(state.get("before").unwrap_or(&Value::Null));
        if current.get("override_sha256") != before.get("override_sha256")
            || current.get("config_sha256") != before.get("config_sha256")
        {
            return Err("Portal changed after preparation".to_string());
        }
        let live_config = get_str(&Value::Object(state.clone()), "live_config")
            .unwrap_or("")
            .to_string();
        if sha(&self.files.read(Path::new(&live_config), 0)?)
            != get_str(&Value::Object(state.clone()), "live_config_sha256").unwrap_or("")
        {
            return Err("Prepared live configuration changed".to_string());
        }
        let plan_sha = state
            .get("plan")
            .and_then(|plan| plan.get("plan_sha256"))
            .and_then(Value::as_str)
            .map(str::to_string);
        self.authorize(job, true, plan_sha.as_deref())?;
        let release = get_str(&Value::Object(state.clone()), "release")
            .unwrap_or("")
            .to_string();
        let command = format!(
            "{} serve --config {}",
            PathBuf::from(&release)
                .join("bin/forge-ide-web-host")
                .display(),
            live_config
        );
        let override_text = format!("[Service]\nExecStart=\nExecStart={command}\n");
        let now = self.clock.wall();
        state.insert("phase".to_string(), Value::String("activating".to_string()));
        state.insert(
            "target_override_sha256".to_string(),
            Value::String(sha(override_text.as_bytes())),
        );
        state.insert("restart_requested".to_string(), json!(true));
        state.insert("startup_started_at".to_string(), json!(now));
        state.insert(
            "startup_deadline_at".to_string(),
            json!(now + STARTUP_IDENTITY_SECONDS),
        );
        state.insert(
            "activation_diagnostic_stage".to_string(),
            Value::String("activation_intent".to_string()),
        );
        self.save(&directory, &state)?;
        let activation = (|| -> Result<(), String> {
            self.files.write(
                &self.config.override_path,
                override_text.as_bytes(),
                0o644,
                0,
            )?;
            self.diagnostic(&directory, &mut state, "service_reload")?;
            self.system.reload()?;
            self.diagnostic(&directory, &mut state, "service_restart")?;
            self.system.restart()?;
            Ok(())
        })();
        if activation.is_err() {
            let _ = self.diagnostic(&directory, &mut state, "activation_call_failed");
            return self.rollback(job, public);
        }
        self.reconcile(job, public)
    }

    pub fn reconcile(&mut self, job: &Value, public: &Value) -> Result<Map<String, Value>, String> {
        let (directory, mut state) = self.load(job, public)?;
        if let Some(outcome) = state.get("outcome") {
            return Ok(map_of(outcome));
        }
        let phase = get_str(&Value::Object(state.clone()), "phase")
            .unwrap_or("")
            .to_string();
        if matches!(phase.as_str(), "staging" | "candidate") {
            if self.authorize(job, true, None).is_err() {
                let release_id = get_str(job, "release_id").unwrap_or("").to_string();
                let _ = self.system.candidate_stop(&release_id);
                return self.rollback(job, public);
            }
            let (packet, _, _) = self.proof(job)?;
            let _ = self
                .system
                .candidate_stop(get_str(job, "release_id").unwrap_or(""));
            return self.finish_stage(&directory, &mut state, &packet);
        }
        if phase == "prepared" {
            if self.authorize(job, true, None).is_err() {
                return self.rollback(job, public);
            }
            return Ok(
                json!({"status": "planned", "plan": state.get("plan").cloned().unwrap_or(Value::Null)})
                    .as_object()
                    .unwrap()
                    .clone(),
            );
        }
        if phase == "rolling_back" {
            return self.rollback(job, public);
        }
        if let Some(current) = self.observe_target(&directory, &mut state) {
            let health = (|| -> Result<(), String> {
                self.diagnostic(&directory, &mut state, "live_health")?;
                let value = map_of(current.get("value").unwrap_or(&Value::Null));
                let inventory = state.get("inventory").cloned().unwrap_or(Value::Null);
                self.live_health(&value, &inventory)
            })();
            if health.is_ok() {
                return self.receipt(
                    &directory,
                    &mut state,
                    "deployed",
                    ReceiptEvidence {
                        health: true,
                        rollback: false,
                        entry: true,
                    },
                    "Exact verified main is active; portal health and admission passed.",
                );
            }
            let _ = self.diagnostic(&directory, &mut state, "live_health_failed");
            return self.rollback(job, public);
        }
        self.rollback(job, public)
    }

    pub fn rollback(&mut self, job: &Value, public: &Value) -> Result<Map<String, Value>, String> {
        let (directory, mut state) = self.load(job, public)?;
        if let Some(outcome) = state.get("outcome") {
            return Ok(map_of(outcome));
        }
        let before = map_of(state.get("before").unwrap_or(&Value::Null));
        let current = self.system.current()?;
        let release_binary =
            PathBuf::from(get_str(&Value::Object(state.clone()), "release").unwrap_or(""))
                .join("bin/forge-ide-web-host")
                .to_string_lossy()
                .into_owned();
        let live_config = get_str(&Value::Object(state.clone()), "live_config")
            .unwrap_or("")
            .to_string();
        let binary = current.get("binary").and_then(Value::as_str).unwrap_or("");
        let config_now = current.get("config").and_then(Value::as_str).unwrap_or("");
        if (binary != before.get("binary").and_then(Value::as_str).unwrap_or("")
            && binary != release_binary)
            || (config_now != before.get("config").and_then(Value::as_str).unwrap_or("")
                && config_now != live_config.as_str())
        {
            return Err("Unrelated portal change prevents automatic rollback".to_string());
        }
        state.insert(
            "phase".to_string(),
            Value::String("rolling_back".to_string()),
        );
        self.save(&directory, &state)?;
        let override_text = before
            .get("override")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        self.files.write(
            &self.config.override_path,
            override_text.as_bytes(),
            0o644,
            0,
        )?;
        self.system.reload()?;
        let pending = |state: &Map<String, Value>, summary: &str, operator_required: bool| {
            let mut receipt = map_of(state.get("plan").unwrap_or(&Value::Null));
            receipt.insert("unit".to_string(), Value::String(UNIT.to_string()));
            receipt.insert("health_confirmed".to_string(), json!(false));
            receipt.insert("cleanup_confirmed".to_string(), json!(false));
            receipt.insert("entry_bridge_confirmed".to_string(), json!(false));
            receipt.insert("rollback_confirmed".to_string(), json!(false));
            receipt.insert("operator_required".to_string(), json!(operator_required));
            json!({"status": "pending", "receipt": receipt, "summary": summary})
                .as_object()
                .unwrap()
                .clone()
        };
        if self.system.restart_pending()? {
            return Ok(pending(
                &state,
                "Reconciling the active portal systemd job before rollback proof.",
                false,
            ));
        }
        let current = self.system.current()?;
        if current.get("process_binary") != before.get("binary") {
            let attempts = match state.get("rollback_restart_attempts") {
                Some(value) if value.is_i64() => value.as_i64().unwrap(),
                _ => {
                    if state.get("rollback_restart_requested") == Some(&Value::Bool(true)) {
                        1
                    } else {
                        0
                    }
                }
            };
            if !(0..=crate::config::ROLLBACK_RESTART_LIMIT).contains(&attempts) {
                return Err("Invalid rollback restart budget".to_string());
            }
            if attempts == crate::config::ROLLBACK_RESTART_LIMIT {
                return Ok(pending(
                    &state,
                    "Rollback restart budget exhausted; operator recovery is required.",
                    true,
                ));
            }
            state.insert("rollback_restart_requested".to_string(), json!(true));
            state.insert("rollback_restart_attempts".to_string(), json!(attempts + 1));
            self.save(&directory, &state)?;
            self.system.restart()?;
        }
        if self.system.restart_pending()? {
            return Ok(pending(
                &state,
                "Reconciling the requested rollback systemd job.",
                false,
            ));
        }
        let restored = self.system.current()?;
        if restored.get("binary") != before.get("binary")
            || restored.get("config") != before.get("config")
            || restored.get("process_binary") != before.get("binary")
        {
            return Ok(pending(
                &state,
                "The previous portal process has not been confirmed yet.",
                false,
            ));
        }
        let old_assets = PathBuf::from(
            before
                .get("value")
                .and_then(|value| get_str(value, "assets"))
                .unwrap_or(""),
        );
        let mut inventory = Map::new();
        for name in CORE {
            let raw = self.files.read(&old_assets.join(name), 8 * 1024 * 1024)?;
            inventory.insert(format!("assets/{name}"), json!({"sha256": sha(&raw)}));
        }
        let before_value = map_of(before.get("value").unwrap_or(&Value::Null));
        self.live_health(&before_value, &Value::Object(inventory))?;
        self.receipt(
            &directory,
            &mut state,
            "failed",
            ReceiptEvidence {
                health: true,
                rollback: true,
                entry: true,
            },
            "Release failed; the previous healthy portal was restored.",
        )
    }

    fn unstaged_failure(&mut self, job: &Value) -> Result<Map<String, Value>, String> {
        let (directory, _) = self.paths(job);
        let current = self.current()?;
        let assets = PathBuf::from(
            current
                .get("value")
                .and_then(|value| get_str(value, "assets"))
                .unwrap_or(""),
        );
        let mut inventory = Map::new();
        for name in CORE {
            let raw = self.files.read(&assets.join(name), 8 * 1024 * 1024)?;
            inventory.insert(format!("assets/{name}"), json!({"sha256": sha(&raw)}));
        }
        let current_value = map_of(current.get("value").unwrap_or(&Value::Null));
        self.live_health(&current_value, &Value::Object(inventory))?;
        let mut body = Map::new();
        body.insert("version".to_string(), json!(1));
        body.insert("policy".to_string(), Value::String(POLICY.to_string()));
        body.insert("job".to_string(), job.clone());
        body.insert("unit".to_string(), Value::String(UNIT.to_string()));
        body.insert("status".to_string(), Value::String("failed".to_string()));
        body.insert("health_confirmed".to_string(), json!(true));
        body.insert("cleanup_confirmed".to_string(), json!(true));
        body.insert("rollback_confirmed".to_string(), json!(true));
        body.insert("entry_bridge_confirmed".to_string(), json!(true));
        body.insert("activation_started".to_string(), json!(false));
        body.insert("process_started".to_string(), json!(false));
        let mut raw = canonical(&Value::Object(body));
        raw.push(b'\n');
        self.files
            .write(&directory.join("activation.json"), &raw, 0o600, 0)?;
        let mut receipt = map_of(job);
        receipt.insert("version".to_string(), json!(1));
        receipt.insert("policy".to_string(), Value::String(POLICY.to_string()));
        receipt.insert("unit".to_string(), Value::String(UNIT.to_string()));
        receipt.insert("health_confirmed".to_string(), json!(true));
        receipt.insert("cleanup_confirmed".to_string(), json!(true));
        receipt.insert("rollback_confirmed".to_string(), json!(true));
        receipt.insert("entry_bridge_confirmed".to_string(), json!(true));
        receipt.insert("process_started".to_string(), json!(false));
        receipt.insert(
            "activation_receipt_sha256".to_string(),
            Value::String(sha(&raw)),
        );
        let outcome = json!({
            "status": "failed",
            "receipt": receipt,
            "summary": "Release stopped before a plan was sealed; the existing healthy portal was retained.",
        });
        let mut state = Map::new();
        state.insert("job".to_string(), job.clone());
        state.insert("phase".to_string(), Value::String("failed".to_string()));
        state.insert("outcome".to_string(), outcome.clone());
        self.save(&directory, &state)?;
        Ok(map_of(&outcome))
    }

    pub fn invoke(
        &mut self,
        action: &str,
        job: &Value,
        public: Option<&Value>,
        authority: Option<&Value>,
    ) -> Result<Map<String, Value>, String> {
        let job_map = self.job(job)?;
        let job = Value::Object(job_map);
        if !matches!(action, "plan" | "activate" | "reconcile" | "rollback") {
            return Err("Unknown release action".to_string());
        }
        let authority = authority.ok_or("Private release lease required")?;
        let authority_object = authority
            .as_object()
            .ok_or("Private release lease required")?;
        let generation = authority_object.get("generation");
        let lease = authority_object.get("lease_token").and_then(Value::as_str);
        if authority_object.len() != 2
            || !generation.map(Value::is_i64).unwrap_or(false)
            || !(1..=100_000).contains(&generation.and_then(Value::as_i64).unwrap_or(0))
            || lease.map(is_canonical_uuid) != Some(true)
        {
            return Err("Private release lease required".to_string());
        }
        self.authority = Some(authority.clone());
        let start = matches!(action, "plan" | "activate");
        if self.authorize(&job, start, None).is_err() {
            if action != "plan" {
                return Err("Release lease or capability is no longer current".to_string());
            }
            self.authorize(&job, false, None)?;
            let (directory, _) = self.paths(&job);
            let path = directory.join("state.json");
            if !path.exists() {
                return self.unstaged_failure(&job);
            }
            let state = map_of(&decode(&self.files.read(&path, 0)?)?);
            if state.get("job") != Some(&job) {
                return Err("Release cleanup identity conflict".to_string());
            }
            if let Some(outcome) = state.get("outcome") {
                return Ok(map_of(outcome));
            }
            if matches!(
                get_str(&Value::Object(state.clone()), "phase"),
                Some("staging" | "candidate")
            ) {
                let _ = self
                    .system
                    .candidate_stop(get_str(&job, "release_id").unwrap_or(""));
            }
            let plan = state.get("plan").cloned().unwrap_or(Value::Null);
            return self.rollback(&job, &plan);
        }
        if action == "plan" {
            if public.is_some() {
                return Err("A release plan is Root-owned".to_string());
            }
            return self.plan(&job);
        }
        let mut public = public.cloned();
        if action == "reconcile" && public.is_none() {
            let (directory, _) = self.paths(&job);
            let path = directory.join("state.json");
            if !path.exists() {
                return self.unstaged_failure(&job);
            }
            let state = map_of(&decode(&self.files.read(&path, 0)?)?);
            if state.get("job") != Some(&job) {
                return Err("Release reconciliation identity conflict".to_string());
            }
            if let Some(outcome) = state.get("outcome") {
                return Ok(map_of(outcome));
            }
            public = state.get("plan").cloned();
        }
        let public = public.ok_or("Frozen release plan required")?;
        if !public.is_object() {
            return Err("Frozen release plan required".to_string());
        }
        match action {
            "activate" => self.activate(&job, &public),
            "reconcile" => self.reconcile(&job, &public),
            "rollback" => self.rollback(&job, &public),
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::Probe;
    use std::os::unix::fs::PermissionsExt;

    /// Test files: the plain filesystem, no Root ownership.
    struct PlainFiles;

    impl RootFiles for PlainFiles {
        fn read(&self, path: &Path, _maximum: usize) -> Result<Vec<u8>, String> {
            std::fs::read(path).map_err(|error| error.to_string())
        }

        fn write(&self, path: &Path, raw: &[u8], mode: u32, _group: u32) -> Result<(), String> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(path, raw).map_err(|error| error.to_string())?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|error| error.to_string())
        }

        fn public_parent(&self, _root: &Path, directory: &Path) -> Result<(), String> {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())
        }
    }

    struct OkProbes;

    impl ProbeProvider for OkProbes {
        fn probe(&self, _origin: &str, _port: u32) -> Box<dyn Probe> {
            Box::new(OkProbe)
        }
    }

    struct FakeSystem {
        current: Map<String, Value>,
        commit: String,
    }

    impl SystemPort for FakeSystem {
        fn call(
            &self,
            _argv: &[String],
            _timeout: f64,
            _data: Option<&[u8]>,
        ) -> Result<Vec<u8>, String> {
            Err("unused".to_string())
        }
        fn show(&self, _key: &str) -> Result<String, String> {
            Err("unused".to_string())
        }
        fn authorize(
            &self,
            _job: &Value,
            _authority: &Value,
            _start: bool,
            _plan: Option<&str>,
        ) -> Result<bool, String> {
            Ok(true)
        }
        fn current(&self) -> Result<Map<String, Value>, String> {
            Ok(self.current.clone())
        }
        fn main_head(&self, _config: &ActorConfig) -> Result<String, String> {
            Ok(self.commit.clone())
        }
        fn reload(&self) -> Result<(), String> {
            Ok(())
        }
        fn restart(&self) -> Result<(), String> {
            Ok(())
        }
        fn restart_pending(&self) -> Result<bool, String> {
            Ok(false)
        }
        fn ensure_entry_bridge(&self) -> Result<(), String> {
            Ok(())
        }
        fn candidate_start(
            &self,
            _config: &ActorConfig,
            _release: &str,
            _binary: &Path,
            _config_path: &Path,
        ) -> Result<(), String> {
            Ok(())
        }
        fn candidate_stop(&self, _release: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct OkProbe;
    impl Probe for OkProbe {
        fn check(
            &mut self,
            _inventory: &Value,
            _project: &str,
        ) -> Result<Map<String, Value>, String> {
            Ok(Map::new())
        }
    }

    #[test]
    fn uuid_shape() {
        assert!(is_canonical_uuid("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!is_canonical_uuid("123E4567-e89b-12d3-a456-426614174000"));
        assert!(!is_canonical_uuid("not-a-uuid"));
    }

    #[test]
    fn plan_seals_a_prepared_release() {
        let root = std::env::temp_dir().join(format!("forge-actor-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ci = root.join("ci");
        let web = root.join("web");
        let releases = root.join("releases");
        let state_root = root.join("state");
        let archive_root = root.join("archive");
        for directory in [&ci, &web, &releases, &state_root, &archive_root] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let commit = "a".repeat(40);
        let tree = "b".repeat(40);
        let action = "123e4567-e89b-12d3-a456-426614174001";
        // The sealed packet.
        let packet = ci.join(format!("{action}.artifacts"));
        std::fs::create_dir_all(packet.join("assets")).unwrap();
        std::fs::create_dir_all(packet.join("bin")).unwrap();
        std::fs::write(packet.join("bin/forge-ide-web-host"), b"binary").unwrap();
        let mut asset_files = Vec::new();
        let mut packet_files: Vec<Value> = vec![json!({
            "path": "bin/forge-ide-web-host",
            "sha256": sha(b"binary"),
            "byte_count": 6,
            "executable": true,
        })];
        let mut total: i64 = 6;
        for name in CORE {
            let content = format!("content-{name}");
            std::fs::write(packet.join("assets").join(name), &content).unwrap();
            asset_files.push(json!({"path": name, "sha256": sha(content.as_bytes())}));
            packet_files.push(json!({
                "path": format!("assets/{name}"),
                "sha256": sha(content.as_bytes()),
                "byte_count": content.len(),
                "executable": false,
            }));
            total += content.len() as i64;
        }
        let assets_manifest = json!({
            "format_version": 1, "abi_version": 18, "files": asset_files,
        });
        let assets_raw = canonical(&assets_manifest);
        std::fs::write(packet.join("assets/manifest.json"), &assets_raw).unwrap();
        packet_files.push(json!({
            "path": "assets/manifest.json",
            "sha256": sha(&assets_raw),
            "byte_count": assets_raw.len(),
            "executable": false,
        }));
        total += assets_raw.len() as i64;
        let manifest = json!({
            "version": 1, "policy": "forge-portal-artifact-v1",
            "action_id": action, "source_commit": commit, "source_tree": tree,
            "request_sha256": "c".repeat(64),
            "commands": artifact_commands(),
            "files": packet_files,
        });
        let manifest_raw = canonical(&manifest);
        std::fs::write(packet.join("manifest.json"), &manifest_raw).unwrap();
        let file_count = manifest["files"].as_array().unwrap().len();
        let result = json!({
            "action_id": action, "candidate_commit": commit, "candidate_tree": tree,
            "expected_main": commit, "gate_manifest_sha256": "d".repeat(64),
            "branch_scope": "main", "verification_profile": "forge-sdlc-jenkins-v2",
            "status": "passed", "version": 2, "cleanup_confirmed": true,
            "checks": main_checks().iter().enumerate().map(|(index, argv)| {
                json!({"index": index, "argv": argv, "returncode": 0})
            }).collect::<Vec<_>>(),
            "coverage": {"scope": "main", "browser": "required",
                "database": "required_zero_skips", "backend_suites": "all_four"},
            "request_sha256": "c".repeat(64),
            "artifact_export": {
                "version": 1, "policy": "forge-portal-artifact-v1",
                "action_id": action, "source_commit": commit, "source_tree": tree,
                "manifest_sha256": sha(&manifest_raw),
                "file_count": file_count, "total_bytes": total,
            },
        });
        let result_raw = canonical(&result);
        std::fs::write(ci.join(format!("{action}.json")), &result_raw).unwrap();
        // The currently-live portal: config + override + replay assets.
        let old_release = releases.join("portal-auto-old");
        let live_config = json!({
            "bind": "127.0.0.1:8788",
            "public_origin": "https://portal.thedevex.com",
            "session_auth": "trusted_proxy",
            "assets": old_release.join("assets").to_string_lossy(),
        });
        std::fs::write(web.join("live.json"), canonical(&live_config)).unwrap();
        std::fs::write(web.join("override.conf"), b"[Service]\nExecStart=\n").unwrap();
        let game_assets = old_release.join("assets/game-assets");
        std::fs::create_dir_all(game_assets.join("levels")).unwrap();
        let room = b"{}";
        let yard = b"{}";
        let game_manifest = json!({
            "entries": {
                "room-one": {"path": "levels/room-one.json", "sha256": sha(room)},
                "traversal-yard": {"path": "levels/traversal-yard.json", "sha256": sha(yard)},
            }
        });
        std::fs::write(game_assets.join("manifest.json"), canonical(&game_manifest)).unwrap();
        std::fs::write(game_assets.join("levels/room-one.json"), room).unwrap();
        std::fs::write(game_assets.join("levels/traversal-yard.json"), yard).unwrap();
        for name in CORE {
            std::fs::write(old_release.join("assets").join(name), format!("old-{name}")).unwrap();
        }
        let group_name = nix::unistd::Group::from_gid(nix::unistd::getegid())
            .ok()
            .flatten()
            .map(|group| group.name)
            .unwrap_or_else(|| "users".to_string());
        let config = ActorConfig {
            owner: "lrocha".to_string(),
            project_alias: "homelab".to_string(),
            repository: "roachmaster/homelab".to_string(),
            canonical_repo: PathBuf::from("/home/lrocha/repos/homelab"),
            ci_results_root: ci.clone(),
            releases_root: releases.clone(),
            web_config_root: web.clone(),
            state_root: state_root.clone(),
            override_path: web.join("override.conf"),
            replay_archive_root: archive_root.clone(),
            web_user: "homelab-web".to_string(),
            web_group: group_name,
            source_account: "lrocha".to_string(),
            source_github_config_dir: PathBuf::from("/home/lrocha/.config/gh"),
            candidate_port: 8789,
            expected_abi: 18,
            mcp_presence: None,
        };
        let mut current = Map::new();
        current.insert(
            "binary".to_string(),
            Value::String(
                old_release
                    .join("bin/forge-ide-web-host")
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        current.insert(
            "config".to_string(),
            Value::String(web.join("live.json").to_string_lossy().into_owned()),
        );
        current.insert("pid".to_string(), json!(0));
        current.insert("process_binary".to_string(), Value::Null);
        current.insert(
            "service_type".to_string(),
            Value::String("simple".to_string()),
        );
        current.insert(
            "active_state".to_string(),
            Value::String("active".to_string()),
        );
        current.insert(
            "sub_state".to_string(),
            Value::String("running".to_string()),
        );
        current.insert("startup_job".to_string(), json!(false));
        let system = FakeSystem {
            current,
            commit: commit.clone(),
        };
        let mut actor = PortalReleaseActor::new(
            config,
            ActorCollaborators {
                system: Box::new(system),
                files: Box::new(PlainFiles),
                probes: Box::new(OkProbes),
                clock: Box::new(crate::forge::SystemActorClock::new()),
            },
        );
        let job = json!({
            "release_id": "123e4567-e89b-12d3-a456-426614174000",
            "verification_id": "123e4567-e89b-12d3-a456-426614174002",
            "owner_id": "lrocha", "project_alias": "homelab",
            "commit_sha": commit, "tree_sha": tree,
            "ci_action_id": action,
            "ci_receipt_sha256": sha(&result_raw),
            "gate_manifest_sha256": "d".repeat(64),
        });
        let authority = json!({
            "generation": 1,
            "lease_token": "123e4567-e89b-12d3-a456-426614174003",
        });
        let outcome = actor
            .invoke("plan", &job, None, Some(&authority))
            .expect("plan succeeds");
        assert_eq!(
            outcome.get("status").and_then(Value::as_str),
            Some("planned")
        );
        let state = map_of(
            &decode(
                &std::fs::read(state_root.join("123e4567-e89b-12d3-a456-426614174000/state.json"))
                    .unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(state.get("phase").and_then(Value::as_str), Some("prepared"));
        std::fs::remove_dir_all(&root).ok();
    }
}
