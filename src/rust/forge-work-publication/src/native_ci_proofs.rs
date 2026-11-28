//! Filesystem proof providers for the native CI recoveries
//! (work_native_ci_postfix_proof.py,
//! work_native_ci_postfix_pass.py and the stopped-context
//! proof in work_native_ci_reprepare.py).
//!
//! Read-only, operator-selected evidence over the
//! publication worker's native-ci state, the root-owned
//! results tree and the deployed verifier helpers. The
//! verifier policy functions are never executed: the
//! CPython AST port (verifier_parity) parses them and a
//! bounded evaluator interprets the two gate predicates
//! over the extracted trees.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use forge_work::canonical::{canonical_json, sha256_hex};
use forge_work::native_ci_completion::{NativePostfixPassEvidence, PostfixPassProofPort};
use forge_work::native_ci_postfix::{
    NativeCiPostfix, NativePostfixEvidence, PostfixProofPort, ReprepareProofPort,
};
use forge_work::WorkError;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::ci_contract::CiGateContractFactory;
use crate::ci_phases::EVENT_PROFILE;
use crate::durable_files::read_private;
use crate::publication_clients::{CommandExecutor, ProcessCommandExecutor};
use crate::strict_json::strict_parse;
use crate::trusted_delivery::{jenkins_gate_manifest, native_action_ids, DeliveryConfig};
use crate::verification::{check_id, protected_retry_observation};
use crate::verifier_parity::{PythonAstPort, VerifierAstPort};

pub const OLD_HELPER: &str =
    "80d0edca103852fca1474245516f2cc56894ebe94857bd5af6d9dd70bd92927d";
pub const M38_PATHS: [&str; 2] = [
    "deploy/mission-control/agent-runner/work_external_plan_override.py",
    "deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql",
];
const NATIVE_VERSION: &str = "forge-native-jenkins-v3";

fn store(message: &str) -> WorkError {
    WorkError::store(message)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn present(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|item| !item.is_null())
}

fn strict(raw: &[u8]) -> Result<Value, WorkError> {
    strict_parse(raw).map_err(|_| store("Invalid strict JSON document"))
}

fn sha256_of(raw: &[u8]) -> String {
    sha256_hex(raw)
}

/// _root_read: every ancestor directory must be root-owned
/// and not group/world-writable before the private read.
pub fn root_read(path: &Path, maximum: usize) -> Result<Vec<u8>, WorkError> {
    use std::os::unix::fs::MetadataExt;
    for parent in path.ancestors().skip(1) {
        let info = std::fs::symlink_metadata(parent)
            .map_err(|_| store("Mutable deployment scope"))?;
        if !info.is_dir() || info.uid() != 0 || info.mode() & 0o022 != 0 {
            return Err(store("Mutable deployment scope"));
        }
    }
    read_private(path, maximum, Some(0)).map_err(WorkError::store)
}

fn root_read_default(path: &Path) -> Result<Vec<u8>, WorkError> {
    root_read(path, 2 * 1024 * 1024)
}

// ---------------------------------------------------------------------------
// Verifier helper analysis (_helper) + the bounded gate evaluator

fn ntype(node: &Value) -> &str {
    node.get("_type").and_then(Value::as_str).unwrap_or("")
}

fn fld<'a>(node: &'a Value, name: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    node.get(name).unwrap_or(&NULL)
}

fn const_str(node: &Value) -> Option<&str> {
    if ntype(node) == "Constant" {
        node.get("value").and_then(Value::as_str)
    } else {
        None
    }
}

fn literal_str_set(node: &Value) -> Option<HashSet<String>> {
    let mut node = node;
    if ntype(node) == "Call" {
        let func = fld(node, "func");
        if ntype(func) != "Name" || fld(func, "id").as_str() != Some("frozenset") {
            return None;
        }
        let args = fld(node, "args").as_array()?;
        if args.len() != 1 {
            return None;
        }
        node = &args[0];
    }
    if !matches!(ntype(node), "Set" | "Tuple" | "List") {
        return None;
    }
    let mut values = HashSet::new();
    for element in fld(node, "elts").as_array()? {
        values.insert(const_str(element)?.to_string());
    }
    Some(values)
}

fn is_name(node: &Value, id: &str) -> bool {
    ntype(node) == "Name" && fld(node, "id").as_str() == Some(id)
}

/// The two extracted gate predicates plus the V2 path set,
/// evaluated without executing verifier code.
struct GatePolicy {
    functions: HashMap<String, Value>,
    v2_paths: HashSet<String>,
}

#[derive(Clone)]
enum AstVal {
    Bool(bool),
    Str(String),
    StrSet(HashSet<String>),
    StrList(Vec<String>),
}

impl GatePolicy {
    fn profile_protects(&self, path: &str, profile: &str) -> Result<bool, WorkError> {
        match self.call(
            "profile_protected_gate_path",
            vec![AstVal::Str(path.to_string()), AstVal::Str(profile.to_string())],
        )? {
            AstVal::Bool(flag) => Ok(flag),
            _ => Err(store("Verifier gate predicate did not return a boolean")),
        }
    }

    fn call(&self, name: &str, args: Vec<AstVal>) -> Result<AstVal, WorkError> {
        let definition = self
            .functions
            .get(name)
            .ok_or_else(|| store("Verifier gate predicate missing"))?;
        let params = fld(fld(definition, "args"), "args");
        let mut env: HashMap<String, AstVal> = HashMap::new();
        for (param, value) in params.as_array().map(|a| a.as_slice()).unwrap_or(&[]).iter().zip(args) {
            env.insert(fld(param, "arg").as_str().unwrap_or("").to_string(), value);
        }
        for statement in fld(definition, "body").as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            match ntype(statement) {
                "Assign" => {
                    let value = self.eval(fld(statement, "value"), &env)?;
                    let targets = fld(statement, "targets");
                    if let Some(target) = targets.as_array().and_then(|t| t.first()) {
                        if ntype(target) == "Name" {
                            env.insert(
                                fld(target, "id").as_str().unwrap_or("").to_string(),
                                value,
                            );
                        }
                    }
                }
                "Return" => return self.eval(fld(statement, "value"), &env),
                _ => return Err(store("Unsupported verifier policy statement")),
            }
        }
        Err(store("Verifier gate predicate did not return"))
    }

    fn eval(&self, node: &Value, env: &HashMap<String, AstVal>) -> Result<AstVal, WorkError> {
        match ntype(node) {
            "Constant" => match &node["value"] {
                Value::String(text) => Ok(AstVal::Str(text.clone())),
                Value::Bool(flag) => Ok(AstVal::Bool(*flag)),
                _ => Err(store("Unsupported verifier policy constant")),
            },
            "Name" => {
                let id = fld(node, "id").as_str().unwrap_or("");
                match id {
                    "EVENT_PROFILE" => Ok(AstVal::Str(EVENT_PROFILE.to_string())),
                    "SCOPED_PROFILE" => Ok(AstVal::Str("forge-sdlc-jenkins-v2".to_string())),
                    "V2_PROTECTED_PATHS" => Ok(AstVal::StrSet(self.v2_paths.clone())),
                    _ => env
                        .get(id)
                        .cloned()
                        .ok_or_else(|| store("Unbound verifier policy name")),
                }
            }
            "Set" => {
                let mut values = HashSet::new();
                for element in fld(node, "elts").as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                    values.insert(
                        const_str(element)
                            .ok_or_else(|| store("Unsupported verifier policy set"))?
                            .to_string(),
                    );
                }
                Ok(AstVal::StrSet(values))
            }
            "BoolOp" => {
                let op = ntype(fld(node, "op"));
                let mut result = op == "And";
                for value in fld(node, "values").as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                    let AstVal::Bool(flag) = self.eval(value, env)? else {
                        return Err(store("Verifier policy operand is not boolean"));
                    };
                    match op {
                        "Or" => result = result || flag,
                        "And" => result = result && flag,
                        _ => return Err(store("Unsupported verifier policy operator")),
                    }
                }
                Ok(AstVal::Bool(result))
            }
            "Compare" => {
                let ops = fld(node, "ops");
                let comparators = fld(node, "comparators");
                let (Some(op), Some(right)) = (
                    ops.as_array().and_then(|a| a.first()),
                    comparators.as_array().and_then(|a| a.first()),
                ) else {
                    return Err(store("Unsupported verifier policy comparison"));
                };
                let left = self.eval(fld(node, "left"), env)?;
                let right = self.eval(right, env)?;
                let outcome = match (ntype(op), &left, &right) {
                    ("Eq", AstVal::Str(a), AstVal::Str(b)) => a == b,
                    ("Eq", AstVal::Bool(a), AstVal::Bool(b)) => a == b,
                    ("In", AstVal::Str(a), AstVal::Str(b)) => b.contains(a.as_str()),
                    ("In", AstVal::Str(a), AstVal::StrSet(set)) => set.contains(a),
                    ("NotIn", AstVal::Str(a), AstVal::Str(b)) => !b.contains(a.as_str()),
                    ("NotIn", AstVal::Str(a), AstVal::StrSet(set)) => !set.contains(a),
                    _ => return Err(store("Unsupported verifier policy comparison")),
                };
                Ok(AstVal::Bool(outcome))
            }
            "Call" => {
                let func = fld(node, "func");
                let args = fld(node, "args");
                let args = args.as_array().map(|a| a.as_slice()).unwrap_or(&[]);
                if ntype(func) == "Name" {
                    if is_name(func, "protected_gate_path") && args.len() == 1 {
                        let arg = self.eval(&args[0], env)?;
                        return self.call("protected_gate_path", vec![arg]);
                    }
                    return Err(store("Unsupported verifier policy call"));
                }
                if ntype(func) == "Attribute" {
                    let base = self.eval(fld(func, "value"), env)?;
                    let attr = fld(func, "attr").as_str().unwrap_or("");
                    let AstVal::Str(base) = base else {
                        return Err(store("Verifier policy method on a non-string"));
                    };
                    match attr {
                        "startswith" if args.len() == 1 => {
                            let AstVal::Str(prefix) = self.eval(&args[0], env)? else {
                                return Err(store("Verifier policy startswith argument"));
                            };
                            Ok(AstVal::Bool(base.starts_with(&prefix)))
                        }
                        "rsplit" if args.len() == 2 => {
                            let AstVal::Str(sep) = self.eval(&args[0], env)? else {
                                return Err(store("Verifier policy rsplit separator"));
                            };
                            let maxsplit = fld(&args[1], "value").as_i64().unwrap_or(0);
                            if sep.is_empty() || maxsplit < 0 {
                                return Err(store("Verifier policy rsplit bound"));
                            }
                            let mut parts: Vec<String> = base
                                .rsplitn((maxsplit + 1) as usize, sep.as_str())
                                .map(|part| part.to_string())
                                .collect();
                            parts.reverse();
                            Ok(AstVal::StrList(parts))
                        }
                        _ => Err(store("Unsupported verifier policy method")),
                    }
                } else {
                    Err(store("Unsupported verifier policy call"))
                }
            }
            "Subscript" => {
                let AstVal::StrList(parts) = self.eval(fld(node, "value"), env)? else {
                    return Err(store("Verifier policy subscript of a non-list"));
                };
                let index = fld(node, "slice")["value"]
                    .as_i64()
                    .ok_or_else(|| store("Verifier policy subscript index"))?;
                let resolved = if index < 0 {
                    parts.len() as i64 + index
                } else {
                    index
                };
                parts
                    .get(resolved as usize)
                    .map(|part| AstVal::Str(part.clone()))
                    .ok_or_else(|| store("Verifier policy subscript out of range"))
            }
            _ => Err(store("Unsupported verifier policy expression")),
        }
    }
}

/// _helper: the CONFIG path, the normalized module tree
/// (the CONFIG literal replaced, as ast.dump equality) and
/// the gate policy of one verifier source.
struct HelperAnalysis {
    config_path: PathBuf,
    normalized: Value,
    policy: GatePolicy,
}

fn analyze_helper(raw: &[u8], port: &dyn VerifierAstPort) -> Result<HelperAnalysis, WorkError> {
    let mut tree = port.parse(raw).map_err(WorkError::store)?;
    let mut config: Option<PathBuf> = None;
    let mut functions: HashMap<String, Value> = HashMap::new();
    let mut v2_paths: HashSet<String> = HashSet::new();
    let body = tree
        .get_mut("body")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| store("Unexpected verifier module"))?;
    for node in body.iter_mut() {
        let targets = fld(node, "targets").clone();
        let target_names: Vec<&str> = targets
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|target| ntype(target) == "Name")
                    .filter_map(|target| fld(target, "id").as_str())
                    .collect()
            })
            .unwrap_or_default();
        if ntype(node) == "Assign" && target_names.contains(&"CONFIG") {
            if config.is_some() {
                return Err(store("Unexpected immutable verifier selector"));
            }
            let value = fld(node, "value");
            let args = fld(value, "args");
            if ntype(value) != "Call"
                || !is_name(fld(value, "func"), "Path")
                || args.as_array().map(|a| a.len()) != Some(1)
            {
                return Err(store("Unexpected immutable verifier selector"));
            }
            let literal = const_str(&args[0])
                .ok_or_else(|| store("Unexpected immutable verifier selector"))?;
            config = Some(PathBuf::from(literal));
            if let Some(args) = node
                .get_mut("value")
                .and_then(|value| value.get_mut("args"))
                .and_then(Value::as_array_mut)
            {
                args[0] = json!({
                    "_type": "Constant",
                    "value": "/etc/forge-ci/pipeline.json",
                    "kind": Value::Null,
                });
            }
        }
        if ntype(node) == "FunctionDef" {
            let name = fld(node, "name").as_str().unwrap_or("");
            if name == "protected_gate_path" || name == "profile_protected_gate_path" {
                functions.insert(name.to_string(), node.clone());
            }
        }
        if ntype(node) == "Assign" && target_names.contains(&"V2_PROTECTED_PATHS") {
            let value = fld(node, "value");
            let args = fld(value, "args");
            if ntype(value) != "Call" || args.as_array().map(|a| a.len()) != Some(1) {
                return Err(store("Unexpected verifier V2 policy"));
            }
            v2_paths = literal_str_set(&args[0])
                .ok_or_else(|| store("Unexpected verifier V2 policy"))?;
        }
    }
    let config = config.ok_or_else(|| store("Incomplete verifier policy proof"))?;
    if !config.is_absolute()
        || config
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        || functions.len() != 2
    {
        return Err(store("Incomplete verifier policy proof"));
    }
    Ok(HelperAnalysis {
        config_path: config,
        normalized: tree,
        policy: GatePolicy {
            functions,
            v2_paths,
        },
    })
}

/// _gate: the v3 gate manifest over a protected-file map.
fn gate_manifest_for(
    files: &BTreeMap<String, Value>,
    helper_sha: &str,
    scope: &str,
) -> Result<String, WorkError> {
    let plan = CiGateContractFactory::new(EVENT_PROFILE)
        .map_err(WorkError::store)?
        .create()
        .plan(scope)
        .map_err(WorkError::store)?;
    let files_value = Value::Object(
        files
            .iter()
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect(),
    );
    let document = json!({
        "version": 3,
        "provider": "jenkins",
        "profile": EVENT_PROFILE,
        "checks": plan.checks,
        "verifier_sha256": helper_sha,
        "source_gate_sha256": sha256_of(canonical_json(&files_value).as_bytes()),
        "branch_scope": scope,
        "phase_plan": plan.phases.iter().map(|phase| json!({
            "phase_id": phase.phase_id,
            "kind": phase.kind,
            "argv": phase.argv,
            "logical_gate": phase.logical_gate,
        })).collect::<Vec<_>>(),
    });
    Ok(sha256_of(canonical_json(&document).as_bytes()))
}

// ---------------------------------------------------------------------------
// ActivePublicationSelector

/// Selects the running publication worker's own immutable
/// runtime module (and re-verifies the worker identity on
/// close, Python's context-manager exit check).
pub struct ActivePublicationSelector {
    execute: std::cell::RefCell<ProcessCommandExecutor>,
}

pub struct ActivePublicationGuard<'a> {
    selector: &'a ActivePublicationSelector,
    pid: i64,
    pub module: PathBuf,
}

impl ActivePublicationSelector {
    pub fn new() -> Self {
        Self {
            execute: std::cell::RefCell::new(ProcessCommandExecutor),
        }
    }

    fn pid(&self) -> Result<i64, WorkError> {
        let argv: Vec<String> = [
            "/usr/bin/systemctl",
            "show",
            "forge-publication-worker.service",
            "--property=MainPID",
            "--value",
        ]
        .iter()
        .map(|item| item.to_string())
        .collect();
        let env = vec![("PATH".to_string(), "/usr/bin:/bin".to_string())];
        let raw = self
            .execute
            .borrow_mut()
            .execute(&argv, &env, None, None, 5.0)
            .map_err(|error| WorkError::store(error.to_string()))?;
        let value = String::from_utf8_lossy(&raw).trim().to_string();
        let pid: i64 = value
            .parse()
            .map_err(|_| store("Publication worker is not active"))?;
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || pid < 2
        {
            return Err(store("Publication worker is not active"));
        }
        Ok(pid)
    }

    pub fn acquire(
        &self,
        publication_path: &Path,
        account_uid: u32,
    ) -> Result<ActivePublicationGuard<'_>, WorkError> {
        use std::os::unix::fs::MetadataExt;
        let pid = self.pid()?;
        let directory = PathBuf::from("/proc").join(pid.to_string());
        let info = std::fs::metadata(&directory)
            .map_err(|_| store("Publication worker account changed"))?;
        if info.uid() != account_uid {
            return Err(store("Publication worker account changed"));
        }
        let argv = std::fs::read(directory.join("cmdline"))
            .map_err(|_| store("Publication command changed"))?;
        if argv.len() > 8192 {
            return Err(store("Oversized publication command"));
        }
        let trimmed: &[u8] = {
            let mut end = argv.len();
            while end > 0 && argv[end - 1] == 0 {
                end -= 1;
            }
            &argv[..end]
        };
        let parts: Vec<&[u8]> = if trimmed.is_empty() {
            Vec::new()
        } else {
            trimmed.split(|byte| *byte == 0).collect()
        };
        if parts.len() != 3 || parts[0] != b"/usr/bin/python3" || parts[1] != b"-B" {
            return Err(store("Publication executable changed"));
        }
        let module = PathBuf::from(
            String::from_utf8(parts[2].to_vec())
                .map_err(|_| store("Publication executable changed"))?,
        );
        let ancestors: Vec<&Path> = module.ancestors().collect();
        if ancestors.len() <= 5
            || ancestors[5].parent() != Some(Path::new("/opt/homelab-forge/releases"))
            || module
                != ancestors[5]
                    .join("source/deploy/mission-control/agent-runner/work_publication.py")
        {
            return Err(store("Publication runtime is not immutable"));
        }
        let _ = root_read_default(&module)?;
        let environ = std::fs::read(directory.join("environ"))
            .map_err(|_| store("Publication environment changed"))?;
        if environ.len() > 65536 {
            return Err(store("Oversized publication environment"));
        }
        let selected: Vec<&[u8]> = environ
            .split(|byte| *byte == 0)
            .filter(|entry| entry.starts_with(b"FORGE_PUBLICATION_CONFIG="))
            .map(|entry| &entry[b"FORGE_PUBLICATION_CONFIG=".len()..])
            .collect();
        if selected.len() != 1 || selected[0] != publication_path.as_os_str().as_encoded_bytes() {
            return Err(store("Publication config is not the active selector"));
        }
        Ok(ActivePublicationGuard {
            selector: self,
            pid,
            module,
        })
    }
}

impl ActivePublicationGuard<'_> {
    /// The exit check: the worker identity must not have
    /// changed during the proof.
    pub fn close(self) -> Result<PathBuf, WorkError> {
        if self.selector.pid()? != self.pid {
            return Err(store("Publication selector changed during proof"));
        }
        Ok(self.module.clone())
    }
}

// ---------------------------------------------------------------------------
// NativeCiStoppedProof (reprepare)

/// The reprepare recovery's proof scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCiProofConfig {
    pub state_root: PathBuf,
    pub owner_uid: u32,
}

impl NativeCiProofConfig {
    pub fn new(state_root: PathBuf, owner_uid: u32) -> Result<Self, String> {
        if !state_root.is_absolute()
            || state_root
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            return Err("Invalid trusted CI proof scope".to_string());
        }
        Ok(Self {
            state_root,
            owner_uid,
        })
    }
}

/// Read only the selected publication scope; never stop or
/// submit a process.
pub struct NativeCiStoppedProof {
    config: NativeCiProofConfig,
}

impl NativeCiStoppedProof {
    pub fn new(config: NativeCiProofConfig) -> Self {
        Self { config }
    }
}

impl ReprepareProofPort for NativeCiStoppedProof {
    fn acquire(
        &self,
        task: &Value,
        review: &Value,
        workflow: &Value,
    ) -> Result<Value, WorkError> {
        let root = &self.config.state_root;
        let read = |path: &Path| -> Result<(Value, String), WorkError> {
            let raw = read_private(path, 262144, Some(self.config.owner_uid))
                .map_err(WorkError::store)?;
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|error| WorkError::store(error.to_string()))?;
            Ok((value, sha256_of(&raw)))
        };
        let reconcile = || WorkError::rejected(409, "reconcile_required");
        let (index, index_sha) = read(
            &root
                .join("reviews")
                .join(format!("{}.json", text(review, "review_id"))),
        )?;
        let keys: HashSet<&str> = index
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        if keys != HashSet::from(["version", "action_id"])
            || !matches!(text(&index, "version"), "forge-native-jenkins-v2" | NATIVE_VERSION)
        {
            return Err(reconcile());
        }
        let action = Uuid::parse_str(text(&index, "action_id"))
            .map_err(|_| reconcile())?
            .to_string();
        let directory = root.join("actions").join(&action);
        let (binding, binding_sha) = read(&directory.join("native-binding.json"))?;
        let (cancel, cancel_sha) = read(&directory.join("cancel-observation.json"))?;
        let expected = json!({
            "owner_id": task["owner_id"],
            "project_alias": task["project_alias"],
            "task_id": text(task, "id"),
            "attempt_id": text(task, "latest_attempt_id"),
            "review_id": text(review, "review_id"),
            "workflow_id": text(workflow, "workflow_id"),
            "correlation_id": text(task, "correlation_id"),
        });
        let request = &binding["request"];
        let receipt = &cancel["receipt"];
        let identity = json!({
            "owner_id": expected["owner_id"],
            "project_alias": expected["project_alias"],
            "task_id": expected["task_id"],
            "attempt_id": expected["attempt_id"],
            "review_id": expected["review_id"],
            "workflow_id": expected["workflow_id"],
            "correlation_id": expected["correlation_id"],
            "candidate_commit": request["candidate_commit"],
            "candidate_tree": request["candidate_tree"],
            "native_gate_manifest_sha256": binding["native_gate_manifest_sha256"],
        });
        let action_ids = native_action_ids(text(&index, "version"), &identity)
            .map_err(|error| WorkError::store(error.to_string()))?;
        let scope_matches = expected
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .all(|(key, value)| &request[key] == value)
            })
            .unwrap_or(false);
        if text(&binding, "version") != text(&index, "version")
            || binding["scope"] != expected
            || !scope_matches
            || text(request, "action_id") != action
            || !action_ids.iter().any(|id| id == &action)
            || text(request, "prepare_action_id")
                != Uuid::new_v5(&Uuid::parse_str(&action).unwrap(), b"candidate-bundle")
                    .to_string()
            || request["candidate_commit"] != review["candidate_commit"]
            || request["integration_base"] != review["verification"]["integration_base"]
            || text(&cancel, "status") != "cancelled"
            || text(receipt, "provider") != "jenkins"
            || text(receipt, "target_action_id") != action
            || receipt["process_started"].as_bool() != Some(false)
            || directory.join("jenkins.json").exists()
        {
            return Err(reconcile());
        }
        Ok(json!({
            "action_id": action,
            "index_sha256": index_sha,
            "binding_sha256": binding_sha,
            "cancel_sha256": cancel_sha,
            "process_started": false,
        }))
    }
}

// ---------------------------------------------------------------------------
// NativePostfixProof

/// The post-fix proof scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePostfixProofConfig {
    pub publication_path: PathBuf,
    pub state_root: PathBuf,
    pub worker_uid: u32,
    pub root_results: PathBuf,
}

impl NativePostfixProofConfig {
    pub fn new(
        publication_path: PathBuf,
        state_root: PathBuf,
        worker_uid: u32,
        root_results: PathBuf,
    ) -> Result<Self, String> {
        let valid = |path: &Path| {
            path.is_absolute()
                && !path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
        };
        if !valid(&publication_path) || !valid(&state_root) || !valid(&root_results) {
            return Err("Invalid private post-fix proof scope".to_string());
        }
        Ok(Self {
            publication_path,
            state_root,
            worker_uid,
            root_results,
        })
    }
}

/// Deployment and failed-CI evidence for the M38 rescue.
pub struct NativePostfixProof {
    config: NativePostfixProofConfig,
    selector: ActivePublicationSelector,
    execute: std::cell::RefCell<ProcessCommandExecutor>,
    ast_port: Box<dyn VerifierAstPort>,
}

impl NativePostfixProof {
    pub fn new(config: NativePostfixProofConfig, selector: ActivePublicationSelector) -> Self {
        Self {
            config,
            selector,
            execute: std::cell::RefCell::new(ProcessCommandExecutor),
            ast_port: Box::new(PythonAstPort),
        }
    }

    fn git(
        &self,
        repository: &Path,
        args: &[String],
    ) -> Result<Vec<u8>, WorkError> {
        let mut argv: Vec<String> = vec![
            "/usr/bin/git".to_string(),
            "-c".to_string(),
            format!("safe.directory={}", repository.display()),
            "-c".to_string(),
            "core.fsmonitor=false".to_string(),
            "-C".to_string(),
            repository.display().to_string(),
        ];
        argv.extend(args.iter().cloned());
        let env: Vec<(String, String)> = [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/nonexistent"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_NO_REPLACE_OBJECTS", "1"),
        ]
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        self.execute
            .borrow_mut()
            .execute(&argv, &env, None, None, 20.0)
            .map_err(|error| WorkError::store(error.to_string()))
    }
}

impl PostfixProofPort for NativePostfixProof {
    fn acquire(
        &self,
        command: &NativeCiPostfix,
        task: &Value,
        review: &Value,
        workflow: &Value,
    ) -> Result<NativePostfixEvidence, WorkError> {
        let c = &self.config;
        let q = review;
        let local = |path: &Path| -> Result<Vec<u8>, WorkError> {
            read_private(path, 262144, Some(c.worker_uid)).map_err(WorkError::store)
        };
        let index_raw = local(
            &c.state_root
                .join("native-ci/reviews")
                .join(format!("{}.json", text(q, "review_id"))),
        )?;
        let index = strict(&index_raw)?;
        let index_keys: HashSet<&str> = index
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        if index_keys != HashSet::from(["version", "action_id"])
            || text(&index, "version") != NATIVE_VERSION
        {
            return Err(store("Wrong failed CI index"));
        }
        let action = Uuid::parse_str(text(&index, "action_id"))
            .map_err(|_| store("Wrong failed CI index"))?
            .to_string();
        let directory = c.state_root.join("native-ci/actions").join(&action);
        let binding_raw = local(&directory.join("native-binding.json"))?;
        let binding = strict(&binding_raw)?;
        let old = &binding["request"];
        let expected = json!({
            "owner_id": task["owner_id"],
            "project_alias": task["project_alias"],
            "task_id": text(task, "id"),
            "attempt_id": text(task, "latest_attempt_id"),
            "review_id": text(q, "review_id"),
            "workflow_id": text(workflow, "workflow_id"),
            "correlation_id": text(task, "correlation_id"),
        });
        let identity = json!({
            "owner_id": expected["owner_id"],
            "project_alias": expected["project_alias"],
            "task_id": expected["task_id"],
            "attempt_id": expected["attempt_id"],
            "review_id": expected["review_id"],
            "workflow_id": expected["workflow_id"],
            "correlation_id": expected["correlation_id"],
            "candidate_commit": old["candidate_commit"],
            "candidate_tree": old["candidate_tree"],
            "native_gate_manifest_sha256": binding["native_gate_manifest_sha256"],
        });
        let action_ids = native_action_ids(NATIVE_VERSION, &identity)
            .map_err(|error| WorkError::store(error.to_string()))?;
        let scope_matches = expected
            .as_object()
            .map(|object| object.iter().all(|(key, value)| &old[key] == value))
            .unwrap_or(false);
        if text(&binding, "version") != text(&index, "version")
            || binding["scope"] != expected
            || !scope_matches
            || text(old, "action_id") != action
            || !action_ids.iter().any(|id| id == &action)
            || text(old, "prepare_action_id")
                != Uuid::new_v5(&Uuid::parse_str(&action).unwrap(), b"candidate-bundle")
                    .to_string()
            || old["candidate_commit"] != q["candidate_commit"]
            || binding["native_gate_manifest_sha256"] != q["gate_manifest_sha256"]
            || old["integration_base"] != q["verification"]["integration_base"]
            || text(&binding, "verifier_sha256") != OLD_HELPER
        {
            return Err(store("Failed CI source binding changed"));
        }
        let observation_raw = local(&directory.join("observation.json"))?;
        let observation = strict(&observation_raw)?;
        let root_raw = root_read(&c.root_results.join(format!("{action}.json")), 262144)?;
        let root = strict(&root_raw)?;
        let digest = sha256_of(&root_raw);
        let receipt = &observation["receipt"];
        let failure = &q["verification"]["delivery_failure"];
        let build = &failure["build"];
        let build_matches = ["provider", "job", "action_id", "queue_id", "build_number"]
            .iter()
            .all(|key| build[*key] == receipt[*key]);
        let root_matches = [
            "action_id",
            "workflow_id",
            "candidate_commit",
            "candidate_tree",
            "gate_manifest_sha256",
            "verification_profile",
            "correlation_id",
        ]
        .iter()
        .all(|key| root[*key] == old[*key]);
        if text(&observation, "status") != "failed"
            || text(receipt, "check_receipt_sha256") != digest
            || receipt["cleanup_confirmed"].as_bool() != Some(true)
            || text(receipt, "failure") != "verification_unavailable"
            || text(&root, "status") != "failed"
            || root["cleanup_confirmed"].as_bool() != Some(true)
            || text(&root, "failure") != "verification_unavailable"
            || root["checks"] != json!([])
            || !root_matches
            || root["expected_main"] != old["integration_base"]
            || root["branch_scope"] != old["branch_scope"]
            || q["finish_receipt"]["outcome"]["retry_observation"]["stage_evidence_digest"]
                != json!(digest)
            || failure["tested_candidate"] != q["candidate_commit"]
            || failure["current_main"] != old["integration_base"]
            || !build_matches
            || text(&root, "log_sha256") != sha256_of(b"")
        {
            return Err(store("Failed CI cleanup or root receipt changed"));
        }
        let journal = strict(&local(&directory.join("jenkins.json"))?)?;
        if journal["post_started"].as_bool() != Some(true)
            || journal["queue_id"] != receipt["queue_id"]
        {
            return Err(store("Actual Jenkins submission proof missing"));
        }
        let guard = self.selector.acquire(&c.publication_path, c.worker_uid)?;
        let module = guard.module.clone();
        let pub_raw = root_read_default(&c.publication_path)?;
        let publication = strict(&pub_raw)?;
        let delivery_path = PathBuf::from(text(&publication, "jenkins_delivery_config"));
        let delivery_raw = read_private(&delivery_path, 65536, Some(c.worker_uid))
            .map_err(WorkError::store)?;
        let delivery_document = strict(&delivery_raw)?;
        let selected = DeliveryConfig::parse(&delivery_document)
            .map_err(|error| WorkError::store(error.to_string()))?;
        let expected_verifier_parent = Path::new("/opt/forge-ci")
            .join(format!("native-verifier-{}", &command.new_helper_sha256[..16]));
        if text(&publication, "hosted_provider") != "jenkins"
            || text(&publication, "jenkins_verification_profile") != EVENT_PROFILE
            || publication
                .get("native_identity_protocol")
                .and_then(Value::as_str)
                .unwrap_or("legacy-v1")
                != "legacy-v1"
            || selected.verification_profile != EVENT_PROFILE
            || selected.owner != text(task, "owner_id")
            || selected.verifier.file_name().and_then(|name| name.to_str())
                != Some("verify-candidate.py")
            || selected.verifier.parent() != Some(expected_verifier_parent.as_path())
            || selected.results_root != c.root_results
        {
            return Err(store("New publication helper selection changed"));
        }
        let new_raw = root_read_default(&selected.verifier)?;
        let new_sha = sha256_of(&new_raw);
        if new_sha != command.new_helper_sha256 || new_sha == OLD_HELPER {
            return Err(store("Selected helper is not the deployed fix"));
        }
        let launcher = root_read_default(Path::new("/usr/local/libexec/forge-native-ci-check"))?;
        if sha256_of(&launcher) != new_sha {
            return Err(store("Actual Jenkins launcher does not use the fixed helper"));
        }
        let new_helper = analyze_helper(&new_raw, self.ast_port.as_ref())?;
        let baseline_path = module
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| store("Publication runtime is not immutable"))?
            .join("jenkins/verify-candidate.py");
        let baseline_raw = root_read_default(&baseline_path)?;
        let baseline = analyze_helper(&baseline_raw, self.ast_port.as_ref())?;
        if new_helper.normalized != baseline.normalized {
            return Err(store("Selected helper differs from qualified runtime policy"));
        }
        let root_config_raw = root_read_default(&new_helper.config_path)?;
        let root_config = strict(&root_config_raw)?;
        if text(&root_config, "verifier_sha256") != new_sha
            || text(&root_config, "verification_profile") != EVENT_PROFILE
        {
            return Err(store(
                "Actual root verifier config does not select the fixed helper",
            ));
        }
        let old_helper_path = Path::new("/opt/forge-ci")
            .join(format!("native-verifier-{}", &OLD_HELPER[..16]))
            .join("verify-candidate.py");
        let old_raw = root_read_default(&old_helper_path)?;
        if sha256_of(&old_raw) != OLD_HELPER {
            return Err(store("Historical helper proof changed"));
        }
        let old_helper = analyze_helper(&old_raw, self.ast_port.as_ref())?;
        let repository = c.state_root.join(text(q, "review_id")).join("repository");
        let repo_info = std::fs::symlink_metadata(&repository)
            .map_err(|_| store("Failed candidate repository is unavailable"))?;
        if repo_info.file_type().is_symlink() || !repo_info.is_dir() {
            return Err(store("Failed candidate repository is unavailable"));
        }
        let candidate = text(q, "candidate_commit");
        let tree_raw = self.git(
            &repository,
            &[
                "rev-parse".to_string(),
                "--verify".to_string(),
                format!("{candidate}^{{tree}}"),
            ],
        )?;
        if String::from_utf8_lossy(&tree_raw).trim() != text(old, "candidate_tree") {
            return Err(store("Failed candidate tree changed"));
        }
        let listing = self.git(
            &repository,
            &[
                "ls-tree".to_string(),
                "-r".to_string(),
                "-z".to_string(),
                "--full-tree".to_string(),
                candidate.to_string(),
            ],
        )?;
        let mut files: BTreeMap<String, Value> = BTreeMap::new();
        for row in listing.split(|byte| *byte == 0) {
            if row.is_empty() {
                continue;
            }
            let row = String::from_utf8_lossy(row).into_owned();
            let Some((descriptor, path)) = row.split_once('\t') else {
                return Err(store("Unsupported failed candidate proof"));
            };
            let fields: Vec<&str> = descriptor.split(' ').collect();
            if fields.len() != 3
                || fields[1] != "blob"
                || !matches!(fields[0], "100644" | "100755")
                || files.len() >= 20000
            {
                return Err(store("Unsupported failed candidate proof"));
            }
            files.insert(path.to_string(), json!([fields[0], fields[2]]));
        }
        let mut old_files: BTreeMap<String, Value> = BTreeMap::new();
        let mut new_files: BTreeMap<String, Value> = BTreeMap::new();
        for (path, entry) in &files {
            if old_helper.policy.profile_protects(path, EVENT_PROFILE)? {
                old_files.insert(path.clone(), entry.clone());
            }
            if new_helper.policy.profile_protects(path, EVENT_PROFILE)? {
                new_files.insert(path.clone(), entry.clone());
            }
        }
        let m38: HashSet<&str> = M38_PATHS.iter().copied().collect();
        let added: HashSet<&str> = new_files
            .keys()
            .map(|key| key.as_str())
            .filter(|key| !old_files.contains_key(*key))
            .collect();
        let removed: HashSet<&str> = old_files
            .keys()
            .map(|key| key.as_str())
            .filter(|key| !new_files.contains_key(*key))
            .collect();
        if added != m38 || !removed.is_empty() {
            return Err(store("Verifier drift is not the exact M38 defect"));
        }
        let branch_scope = text(old, "branch_scope");
        if gate_manifest_for(&new_files, OLD_HELPER, branch_scope)?
            != text(old, "gate_manifest_sha256")
            || gate_manifest_for(&old_files, OLD_HELPER, branch_scope)?
                == text(old, "gate_manifest_sha256")
        {
            return Err(store("Historical gate mismatch was not reproduced"));
        }
        let expected_new = gate_manifest_for(&new_files, &new_sha, branch_scope)?;
        let produced = jenkins_gate_manifest(
            &selected,
            &files,
            old["branch_scope"].as_str(),
            true,
        )
        .map_err(|error| WorkError::store(error.to_string()))?;
        if produced.2 != expected_new {
            return Err(store(
                "Fixed producer and actual helper policy still differ",
            ));
        }
        let proof = json!({
            "cleanup_confirmed": true,
            "checks_executed": 0,
            "matched_m38_policy": true,
            "old_action_id": action,
            "old_helper_sha256": OLD_HELPER,
            "old_receipt_sha256": digest,
            "index_sha256": sha256_of(&index_raw),
            "binding_sha256": sha256_of(&binding_raw),
            "publication_config_sha256": sha256_of(&pub_raw),
            "delivery_config_sha256": sha256_of(&delivery_raw),
            "root_config_sha256": sha256_of(&root_config_raw),
            "new_helper_sha256": new_sha,
            "new_gate_manifest_sha256": expected_new,
            "launcher_sha256": new_sha,
            "deployed_fix_revision": command.deployed_fix_revision,
            "active_runtime_sha256": sha256_of(&root_read_default(&module)?),
        });
        let module = guard.close()?;
        let _ = module;
        NativePostfixEvidence::new(
            Uuid::parse_str(text(q, "review_id")).map_err(|e| store(&e.to_string()))?,
            text(q, "candidate_commit").to_string(),
            digest,
            command.deployed_fix_revision.clone(),
            new_sha,
            proof,
        )
        .map_err(WorkError::store)
    }
}

// ---------------------------------------------------------------------------
// NativePostfixPassProof

/// The post-fix pass proof scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePostfixPassConfig {
    pub state_root: PathBuf,
    pub spool_root: PathBuf,
    pub results_root: PathBuf,
    pub worker_uid: u32,
}

impl NativePostfixPassConfig {
    pub fn new(
        state_root: PathBuf,
        spool_root: PathBuf,
        results_root: PathBuf,
        worker_uid: u32,
    ) -> Result<Self, String> {
        let valid = |path: &Path| {
            path.is_absolute()
                && !path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
        };
        if !valid(&state_root) || !valid(&spool_root) || !valid(&results_root) {
            return Err("Invalid private post-fix pass proof scope".to_string());
        }
        Ok(Self {
            state_root,
            spool_root,
            results_root,
            worker_uid,
        })
    }
}

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() && !value.is_boolean()
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Root-owned passed-CI evidence for the completion
/// resolver.
pub struct NativePostfixPassProof {
    config: NativePostfixPassConfig,
}

impl NativePostfixPassProof {
    pub fn new(config: NativePostfixPassConfig) -> Self {
        Self { config }
    }
}

impl PostfixPassProofPort for NativePostfixPassProof {
    fn acquire(
        &self,
        review: &Value,
        task: &Value,
        workflow: &Value,
        expected_helper_sha: &str,
    ) -> Result<NativePostfixPassEvidence, WorkError> {
        let c = &self.config;
        // Python validates the scope binding first.
        if review["owner_id"] != task["owner_id"]
            || review["project_alias"] != task["project_alias"]
            || review["owner_id"] != workflow["owner_id"]
            || review["project_alias"] != workflow["project_alias"]
            || review["task_id"] != task["id"]
            || task["workflow_id"] != workflow["workflow_id"]
            || text(review, "phase") != "local"
            || !present(review, "finish_token")
        {
            return Err(store("Historical CI source scope changed"));
        }
        let candidate = text(review, "candidate_commit");
        if !is_hex(candidate, 40)
            || !is_hex(text(review, "gate_manifest_sha256"), 64)
            || !is_hex(expected_helper_sha, 64)
        {
            return Err(store("Historical CI source scope changed"));
        }
        let local = |path: &Path| -> Result<Vec<u8>, WorkError> {
            read_private(path, 262144, Some(c.worker_uid)).map_err(WorkError::store)
        };
        let index_raw = local(
            &c.state_root
                .join("native-ci/reviews")
                .join(format!("{}.json", text(review, "review_id"))),
        )?;
        let index = strict(&index_raw)?;
        let index_keys: HashSet<&str> = index
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        if index_keys != HashSet::from(["version", "action_id"])
            || text(&index, "version") != NATIVE_VERSION
        {
            return Err(store("Passed CI index changed"));
        }
        let action = Uuid::parse_str(text(&index, "action_id"))
            .map_err(|_| store("Passed CI index changed"))?
            .to_string();
        let directory = c.state_root.join("native-ci/actions").join(&action);
        let binding_raw = local(&directory.join("native-binding.json"))?;
        let binding = strict(&binding_raw)?;
        let request = &binding["request"];
        let scope = json!({
            "owner_id": task["owner_id"],
            "project_alias": task["project_alias"],
            "task_id": task["id"],
            "attempt_id": review["attempt_id"],
            "review_id": review["review_id"],
            "workflow_id": workflow["workflow_id"],
            "correlation_id": task["correlation_id"],
        });
        let identity = json!({
            "owner_id": scope["owner_id"],
            "project_alias": scope["project_alias"],
            "task_id": scope["task_id"],
            "attempt_id": scope["attempt_id"],
            "review_id": scope["review_id"],
            "workflow_id": scope["workflow_id"],
            "correlation_id": scope["correlation_id"],
            "candidate_commit": request["candidate_commit"],
            "candidate_tree": request["candidate_tree"],
            "native_gate_manifest_sha256": review["gate_manifest_sha256"],
        });
        let action_ids = native_action_ids(NATIVE_VERSION, &identity)
            .map_err(|error| WorkError::store(error.to_string()))?;
        let scope_matches = scope
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .all(|(key, value)| &request[key] == value)
            })
            .unwrap_or(false);
        if text(&binding, "version") != NATIVE_VERSION
            || binding["scope"] != scope
            || !scope_matches
            || text(request, "action_id") != action
            || !action_ids.iter().any(|id| id == &action)
            || text(request, "prepare_action_id")
                != Uuid::new_v5(&Uuid::parse_str(&action).unwrap(), b"candidate-bundle")
                    .to_string()
            || text(request, "candidate_commit") != candidate
            || text(request, "verification_profile") != EVENT_PROFILE
            || binding["native_gate_manifest_sha256"] != review["gate_manifest_sha256"]
            || text(&binding, "verifier_sha256") != expected_helper_sha
        {
            return Err(store("Passed CI source binding changed"));
        }
        let helper_path = Path::new("/opt/forge-ci")
            .join(format!("native-verifier-{}", &expected_helper_sha[..16]))
            .join("verify-candidate.py");
        let helper_raw = root_read_default(&helper_path)?;
        if sha256_of(&helper_raw) != expected_helper_sha {
            return Err(store("Selected post-fix helper changed"));
        }
        let plan = CiGateContractFactory::new(EVENT_PROFILE)
            .map_err(WorkError::store)?
            .create()
            .plan(text(request, "branch_scope"))
            .map_err(WorkError::store)?;
        let expected = json!({
            "version": plan.result_version(),
            "action_id": action,
            "workflow_id": text(workflow, "workflow_id"),
            "project_alias": text(task, "project_alias"),
            "candidate_commit": candidate,
            "candidate_tree": request["candidate_tree"],
            "expected_main": request["integration_base"],
            "branch_scope": request["branch_scope"],
            "gate_manifest_sha256": request["gate_manifest_sha256"],
            "verification_profile": EVENT_PROFILE,
            "correlation_id": text(task, "correlation_id"),
        });
        if !is_hex(text(request, "integration_base"), 40)
            || !is_hex(text(request, "gate_manifest_sha256"), 64)
        {
            return Err(store("Passed CI source binding changed"));
        }
        let submitted_raw = local(&c.spool_root.join(&action).join("request.json"))?;
        let submitted = strict(&submitted_raw)?;
        let submitted_keys: HashSet<&str> = submitted
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        let mut wanted_keys: HashSet<&str> = expected
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        wanted_keys.insert("bundle_sha256");
        let submitted_matches = expected
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .all(|(key, value)| &submitted[key] == value)
            })
            .unwrap_or(false);
        if submitted_keys != wanted_keys
            || !is_int(&submitted["version"])
            || !submitted_matches
            || !is_hex(text(&submitted, "bundle_sha256"), 64)
        {
            return Err(store("Submitted CI request changed"));
        }
        let request_sha = sha256_of(&submitted_raw);
        let root_raw = root_read(&c.results_root.join(format!("{action}.json")), 262144)?;
        let root = strict(&root_raw)?;
        let digest = sha256_of(&root_raw);
        let root_matches = expected
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .filter(|(key, _)| key.as_str() != "project_alias")
                    .all(|(key, value)| &root[key] == value)
            })
            .unwrap_or(false);
        let checks_ok = root["checks"].as_array().is_some_and(|checks| {
            checks.len() == plan.checks.len()
                && checks.iter().enumerate().all(|(position, check)| {
                    is_int(&check["index"])
                        && check["index"].as_i64() == Some(position as i64)
                        && check["argv"] == json!(plan.checks[position])
                        && is_int(&check["returncode"])
                        && check["returncode"].as_i64() == Some(0)
                })
        });
        if !is_int(&root["version"])
            || text(&root, "status") != "passed"
            || !root["failure"].is_null()
            || root["cleanup_confirmed"].as_bool() != Some(true)
            || !root_matches
            || text(&root, "request_sha256") != request_sha
            || root["coverage"] != plan.coverage().unwrap_or(Value::Null)
            || !checks_ok
            || !is_hex(text(&root, "log_sha256"), 64)
        {
            return Err(store("Root CI receipt changed"));
        }
        let witness = protected_retry_observation(
            &plan.profile,
            &plan.scope,
            Some(&root),
            Some(&digest),
        )
        .map_err(WorkError::store)?;
        if witness
            .as_ref()
            .is_some_and(|value| !value["failure_kind"].is_null())
        {
            return Err(store("Root CI receipt is not a clean pass witness"));
        }
        let observation_raw = local(&directory.join("observation.json"))?;
        let observation = strict(&observation_raw)?;
        let receipt = &observation["receipt"];
        let receipt_matches = expected
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .filter(|(key, _)| key.as_str() != "project_alias")
                    .all(|(key, value)| &receipt[key] == value)
            })
            .unwrap_or(false);
        if text(&observation, "status") != "passed"
            || text(receipt, "provider") != "jenkins"
            || text(receipt, "job") != "forge-native-candidate"
            || text(receipt, "result") != "SUCCESS"
            || text(receipt, "check_receipt_sha256") != digest
            || receipt["cleanup_confirmed"].as_bool() != Some(true)
            || !receipt["failure"].is_null()
            || receipt["checks"] != root["checks"]
            || receipt["log_sha256"] != root["log_sha256"]
            || !receipt_matches
            || receipt["result_version"] != json!(plan.result_version())
            || receipt["coverage"] != plan.coverage().unwrap_or(Value::Null)
        {
            return Err(store("CI observation changed"));
        }
        let journal = strict(&local(&directory.join("jenkins.json"))?)?;
        let build_number = journal["receipt"]["build_number"].clone();
        let queue_ok = journal["receipt"]["queue_id"].is_null()
            || (is_int(&journal["receipt"]["queue_id"])
                && journal["receipt"]["queue_id"].as_i64().is_some_and(|n| n >= 1));
        let parameters = json!({
            "FORGE_ACTION_ID": action,
            "FORGE_WORKFLOW_ID": text(workflow, "workflow_id"),
            "FORGE_CANDIDATE_COMMIT": candidate,
            "FORGE_CANDIDATE_TREE": request["candidate_tree"],
            "FORGE_EXPECTED_MAIN": request["integration_base"],
            "FORGE_GATE_MANIFEST": request["gate_manifest_sha256"],
            "FORGE_CORRELATION_ID": text(task, "correlation_id"),
        });
        if journal["post_started"].as_bool() != Some(true)
            || !is_int(&build_number)
            || build_number.as_i64().is_none_or(|n| n < 1)
            || journal["journal"]["build_number"] != build_number
            || journal["journal"]["queue_id"] != journal["receipt"]["queue_id"]
            || journal["journal"]["parameters"] != parameters
            || !queue_ok
        {
            return Err(store("Actual Jenkins submission proof missing"));
        }
        let verification = &review["verification"];
        let finish = &review["finish_receipt"];
        let outcome = &finish["outcome"];
        let supplied = &outcome["verification"];
        let projected_ok = verification["checks"].as_array().is_some_and(|rows| {
            rows.len() == plan.checks.len()
                && rows.iter().enumerate().all(|(position, row)| {
                    let argv = &plan.checks[position];
                    row["check_id"] == json!(check_id(argv, position))
                        && row["command"] == json!(argv)
                        && text(row, "status") == "passed"
                        && text(row, "head_sha") == candidate
                })
        });
        let supplied_subset = supplied
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .all(|(key, value)| &verification[key] == value)
            })
            .unwrap_or(false);
        if verification["checks_passed"].as_bool() != Some(true)
            || verification["cleanup_confirmed"].as_bool() != Some(true)
            || verification["integration_base"] != request["integration_base"]
            || text(finish, "review_id") != text(review, "review_id")
            || text(finish, "status") != "awaiting_review"
            || text(outcome, "disposition") != "awaiting_review"
            || outcome["candidate_commit"] != review["candidate_commit"]
            || outcome["gate_manifest_sha256"] != review["gate_manifest_sha256"]
            || outcome["retry_observation"]["stage_evidence_digest"] != json!(digest)
            || supplied["checks_passed"].as_bool() != Some(true)
            || supplied["cleanup_confirmed"].as_bool() != Some(true)
            || !supplied_subset
            || !projected_ok
        {
            return Err(store("Review finish does not bind the passed CI"));
        }
        let proof = json!({
            "checks_passed": true,
            "cleanup_confirmed": true,
            "check_count": plan.checks.len(),
            "action_id": action,
            "build_number": build_number,
            "helper_sha256": expected_helper_sha,
            "root_receipt_sha256": digest,
            "request_sha256": request_sha,
            "index_sha256": sha256_of(&index_raw),
            "binding_sha256": sha256_of(&binding_raw),
            "observation_sha256": sha256_of(&observation_raw),
        });
        NativePostfixPassEvidence::new(
            Uuid::parse_str(text(review, "review_id")).map_err(|e| store(&e.to_string()))?,
            Uuid::parse_str(text(task, "id")).map_err(|e| store(&e.to_string()))?,
            Uuid::parse_str(text(review, "attempt_id")).map_err(|e| store(&e.to_string()))?,
            candidate.to_string(),
            digest,
            expected_helper_sha.to_string(),
            proof,
        )
        .map_err(WorkError::store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation() {
        assert!(NativeCiProofConfig::new(PathBuf::from("/srv/state"), 1000).is_ok());
        assert!(NativeCiProofConfig::new(PathBuf::from("relative"), 1000).is_err());
        assert!(
            NativeCiProofConfig::new(PathBuf::from("/srv/../state"), 1000).is_err()
        );
        assert!(NativePostfixProofConfig::new(
            PathBuf::from("/etc/pub.json"),
            PathBuf::from("/srv/state"),
            1000,
            PathBuf::from("/srv/results"),
        )
        .is_ok());
        assert!(NativePostfixProofConfig::new(
            PathBuf::from("pub.json"),
            PathBuf::from("/srv/state"),
            1000,
            PathBuf::from("/srv/results"),
        )
        .is_err());
        assert!(NativePostfixPassConfig::new(
            PathBuf::from("/srv/state"),
            PathBuf::from("/srv/spool"),
            PathBuf::from("/srv/results"),
            1000,
        )
        .is_ok());
        assert!(NativePostfixPassConfig::new(
            PathBuf::from("/srv/state"),
            PathBuf::from("/srv/../spool"),
            PathBuf::from("/srv/results"),
            1000,
        )
        .is_err());
    }

    #[test]
    fn scalar_helpers() {
        assert!(is_int(&json!(3)));
        assert!(!is_int(&json!(true)));
        assert!(!is_int(&json!(1.5)));
        assert!(is_hex(&"a".repeat(40), 40));
        assert!(!is_hex(&"A".repeat(40), 40));
        assert!(!is_hex(&"a".repeat(39), 40));
        assert!(present(&json!({"finish_token": "x"}), "finish_token"));
        assert!(!present(&json!({"finish_token": null}), "finish_token"));
        assert!(!present(&json!({}), "finish_token"));
    }
}
