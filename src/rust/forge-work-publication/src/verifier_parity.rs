//! Verifier policy parity: prove an installed verifier's declared
//! policy matches this gate contract, without executing verifier
//! code. Port of work_ci_contract.py's parity machinery
//! (extract_verifier_policy, _verify_source_policy_parity,
//! _verify_contract_policy_parity and the bounded static evaluator
//! for event_phases_for_scope).
//!
//! Python's implementation walks the CPython `ast` of the verifier
//! source. The Rust port keeps exactly that ground truth: source is
//! parsed by CPython itself through the AST port (a fixed dump
//! script, in the spirit of work_verification_init's fixed
//! launcher), and every extraction, comparison and bounded
//! evaluation rule below is Rust. The verifier file stays Python
//! (src/rust/scripts/jenkins/verify-candidate.py is outside
//! the agent-runner migration map), so the parity proof must keep
//! reading Python source.

use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::ci_contract::{
    legacy_checks, scoped_branch_checks, scoped_main_checks, CiGateContract, CiGateContractFactory,
    CONTRACT_PATH, LEGACY_PROFILE, SCOPED_PROFILE, V2_PROTECTED_PATHS, V3_PROTECTED_PATHS,
    V3_PROTECTED_PREFIXES,
};
use crate::ci_phases::{
    event_phases_for_scope, CiPhase, ALL_FEATURES, EVENT_PROFILE, FEATURE_VARIANTS,
};

const MISMATCH: &str = "Installed verifier policy does not match active CI gate contract";

/// Parse Python source into its JSON AST (node objects carry a
/// `_type` field plus their ast fields; scalar constants stay
/// JSON-native, floats/bytes are tagged objects).
pub trait VerifierAstPort {
    fn parse(&self, source: &[u8]) -> Result<Value, String>;
}

const AST_DUMP_SCRIPT: &str = r"
import sys, ast, json
def conv(node):
    if isinstance(node, ast.AST):
        out = {'_type': type(node).__name__}
        for field in node._fields:
            out[field] = conv(getattr(node, field))
        return out
    if isinstance(node, list):
        return [conv(item) for item in node]
    if isinstance(node, (str, bool, int)) or node is None:
        return node
    if isinstance(node, float):
        return {'$float': repr(node)}
    if isinstance(node, bytes):
        return {'$bytes': node.hex()}
    return {'$other': type(node).__name__}
try:
    tree = ast.parse(sys.stdin.buffer.read().decode('utf-8'))
except Exception as error:
    sys.stderr.write(str(error))
    sys.exit(1)
sys.stdout.write(json.dumps(conv(tree)))
";

/// The production AST port: CPython's own parser via a fixed script.
pub struct PythonAstPort;

/// AST parser adapter backed by any bounded process-exec `IoClient`.
pub struct IoClientAstPort<C> {
    client: C,
}

impl<C> IoClientAstPort<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

impl<C> VerifierAstPort for IoClientAstPort<C>
where
    C: forge_core::io_client::IoClient<
            Request = forge_work_worker::process_exec::ProcessRequest,
            Response = forge_work_worker::process_exec::ProcessOutput,
            Error = forge_work_worker::WorkerError,
        > + Send
        + Sync,
{
    fn parse(&self, source: &[u8]) -> Result<Value, String> {
        let request = forge_work_worker::process_exec::ProcessRequest::inherited(
            vec![
                "/usr/bin/python3".to_string(),
                "-B".to_string(),
                "-c".to_string(),
                AST_DUMP_SCRIPT.to_string(),
            ],
            30,
        )
        .with_stdin(source.to_vec());
        let output = self
            .client
            .execute(request)
            .map_err(|error| format!("{error:?}"))?;
        if output.exit_code != 0 {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
    }
}

impl VerifierAstPort for PythonAstPort {
    fn parse(&self, source: &[u8]) -> Result<Value, String> {
        IoClientAstPort::new(forge_work_worker::process_exec::SystemProcessExec).parse(source)
    }
}

// ---------------------------------------------------------------------------
// AST helpers

fn ntype(node: &Value) -> &str {
    node.get("_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn field<'a>(node: &'a Value, name: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    node.get(name).unwrap_or(&NULL)
}

fn walk_nodes(node: &Value, visit: &mut dyn FnMut(&Value)) {
    match node {
        Value::Object(map) => {
            if map.contains_key("_type") {
                visit(node);
            }
            for value in map.values() {
                walk_nodes(value, visit);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_nodes(item, visit);
            }
        }
        _ => {}
    }
}

fn const_str(node: &Value) -> Option<&str> {
    if ntype(node) == "Constant" {
        field(node, "value").as_str()
    } else {
        None
    }
}

fn is_name(node: &Value, id: &str) -> bool {
    ntype(node) == "Name" && field(node, "id").as_str() == Some(id)
}

// ---------------------------------------------------------------------------
// Pure constant values (ast.literal_eval's subset)

#[derive(Debug, Clone, PartialEq)]
enum PVal {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    None,
    Tuple(Vec<PVal>),
    List(Vec<PVal>),
    Set(Vec<PVal>),
}

fn pval_key(value: &PVal) -> String {
    format!("{value:?}")
}

fn make_set(mut items: Vec<PVal>) -> PVal {
    items.sort_by_key(pval_key);
    items.dedup();
    PVal::Set(items)
}

fn literal_eval(node: &Value) -> Result<PVal, ()> {
    match ntype(node) {
        "Constant" => {
            let value = field(node, "value");
            match value {
                Value::String(text) => Ok(PVal::Str(text.clone())),
                Value::Bool(flag) => Ok(PVal::Bool(*flag)),
                Value::Null => Ok(PVal::None),
                Value::Number(number) => {
                    if let Some(int) = number.as_i64() {
                        Ok(PVal::Int(int))
                    } else if let Some(uint) = number.as_u64() {
                        Ok(PVal::Int(uint as i64))
                    } else {
                        Ok(PVal::Float(number.as_f64().ok_or(())?))
                    }
                }
                Value::Object(map) => {
                    if let Some(text) = map.get("$float").and_then(|v| v.as_str()) {
                        text.parse::<f64>().map(PVal::Float).map_err(|_| ())
                    } else {
                        Err(())
                    }
                }
                _ => Err(()),
            }
        }
        "Tuple" | "List" | "Set" => {
            let items = field(node, "elts")
                .as_array()
                .ok_or(())?
                .iter()
                .map(literal_eval)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(match ntype(node) {
                "Tuple" => PVal::Tuple(items),
                "List" => PVal::List(items),
                _ => make_set(items),
            })
        }
        "UnaryOp" => {
            let operand = literal_eval(field(node, "operand"))?;
            match (ntype(field(node, "op")), operand) {
                ("USub", PVal::Int(value)) => Ok(PVal::Int(-value)),
                ("UAdd", PVal::Int(value)) => Ok(PVal::Int(value)),
                ("USub", PVal::Float(value)) => Ok(PVal::Float(-value)),
                ("UAdd", PVal::Float(value)) => Ok(PVal::Float(value)),
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

/// work_ci_contract._eval_pure_constant: a top-level frozenset(...)
/// call unwraps to a set; anything else goes through literal_eval.
fn eval_pure_constant(node: &Value) -> Result<PVal, ()> {
    if ntype(node) == "Call"
        && is_name(field(node, "func"), "frozenset")
        && field(node, "args")
            .as_array()
            .is_some_and(|args| args.len() == 1)
        && field(node, "keywords")
            .as_array()
            .is_some_and(|keywords| keywords.is_empty())
    {
        let value = literal_eval(&field(node, "args")[0])?;
        return match value {
            PVal::Set(items) => Ok(PVal::Set(items)),
            PVal::Tuple(items) | PVal::List(items) => Ok(make_set(items)),
            _ => Err(()),
        };
    }
    literal_eval(node)
}

fn pval_str_set(value: &PVal) -> Option<HashSet<String>> {
    match value {
        PVal::Set(items) => {
            let mut set = HashSet::new();
            for item in items {
                match item {
                    PVal::Str(text) => {
                        set.insert(text.clone());
                    }
                    _ => return None,
                }
            }
            Some(set)
        }
        _ => None,
    }
}

fn commands_pval(rows: &[Vec<String>]) -> PVal {
    PVal::Tuple(
        rows.iter()
            .map(|row| PVal::Tuple(row.iter().map(|item| PVal::Str(item.clone())).collect()))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// extract_verifier_policy

#[derive(Debug, Default)]
pub struct VerifierPolicy {
    pub checks: Option<Vec<Vec<String>>>,
    pub scoped_branch_checks: Option<Vec<Vec<String>>>,
    pub scoped_main_checks: Option<Vec<Vec<String>>>,
    pub v2_protected_paths: Option<HashSet<String>>,
    pub v3_protected_paths: Option<HashSet<String>>,
    pub v3_protected_prefixes: HashSet<String>,
    pub feature_variants: Option<Vec<(String, String, String, bool)>>,
}

fn literal_str_set(node: &Value) -> Option<HashSet<String>> {
    let mut node = node;
    if ntype(node) == "Call" {
        if !is_name(field(node, "func"), "frozenset")
            || !field(node, "args")
                .as_array()
                .is_some_and(|args| args.len() == 1)
            || !field(node, "keywords")
                .as_array()
                .is_some_and(|keywords| keywords.is_empty())
        {
            return None;
        }
        node = &field(node, "args")[0];
    }
    if !matches!(ntype(node), "Set" | "Tuple" | "List") {
        return None;
    }
    let mut values = HashSet::new();
    for element in field(node, "elts").as_array()? {
        values.insert(const_str(element)?.to_string());
    }
    Some(values)
}

fn literal_commands(node: &Value) -> Option<Vec<Vec<String>>> {
    if ntype(node) != "Tuple" {
        return None;
    }
    let mut commands = Vec::new();
    for item in field(node, "elts").as_array()? {
        if ntype(item) != "Tuple" {
            return None;
        }
        let mut argv = Vec::new();
        for token in field(item, "elts").as_array()? {
            argv.push(const_str(token)?.to_string());
        }
        commands.push(argv);
    }
    Some(commands)
}

fn literal_feature_variants(node: &Value) -> Option<Vec<(String, String, String, bool)>> {
    if ntype(node) != "Tuple" {
        return None;
    }
    let mut variants = Vec::new();
    for item in field(node, "elts").as_array()? {
        if ntype(item) != "Tuple" {
            return None;
        }
        let elts = field(item, "elts").as_array()?;
        if elts.len() != 4 {
            return None;
        }
        let mut strings = Vec::new();
        for element in &elts[..3] {
            strings.push(const_str(element)?.to_string());
        }
        let lint = match field(&elts[3], "value") {
            Value::Bool(flag) if ntype(&elts[3]) == "Constant" => *flag,
            _ => return None,
        };
        variants.push((
            strings[0].clone(),
            strings[1].clone(),
            strings[2].clone(),
            lint,
        ));
    }
    Some(variants)
}

/// Parse installed verifier policy declarations without executing
/// verifier code.
pub fn extract_verifier_policy(
    source: &[u8],
    port: &dyn VerifierAstPort,
) -> Result<VerifierPolicy, String> {
    if std::str::from_utf8(source).is_err() {
        return Err("Installed verifier source is not valid UTF-8 Python".to_string());
    }
    let tree = port
        .parse(source)
        .map_err(|_| "Installed verifier source is not valid UTF-8 Python".to_string())?;
    let mut policy = VerifierPolicy::default();
    let body = field(&tree, "body").as_array().cloned().unwrap_or_default();
    for statement in &body {
        if ntype(statement) == "Assign"
            && field(statement, "targets")
                .as_array()
                .is_some_and(|targets| targets.len() == 1)
            && ntype(&field(statement, "targets")[0]) == "Name"
        {
            let name = field(&field(statement, "targets")[0], "id")
                .as_str()
                .unwrap_or_default();
            let value = field(statement, "value");
            match name {
                "CHECKS" => policy.checks = literal_commands(value),
                "SCOPED_BRANCH_CHECKS" => policy.scoped_branch_checks = literal_commands(value),
                "SCOPED_MAIN_CHECKS" => policy.scoped_main_checks = literal_commands(value),
                "V2_PROTECTED_PATHS" => policy.v2_protected_paths = literal_str_set(value),
                "V3_PROTECTED_PATHS" => policy.v3_protected_paths = literal_str_set(value),
                "FEATURE_VARIANTS" => policy.feature_variants = literal_feature_variants(value),
                _ => {}
            }
        } else if ntype(statement) == "FunctionDef"
            && field(statement, "name").as_str() == Some("profile_protected_gate_path")
        {
            let mut prefixes = HashSet::new();
            let mut extracted_paths: Vec<HashSet<String>> = Vec::new();
            walk_nodes(statement, &mut |node| {
                if ntype(node) == "Compare"
                    && is_name(field(node, "left"), "path")
                    && field(node, "ops")
                        .as_array()
                        .is_some_and(|ops| ops.len() == 1 && ntype(&ops[0]) == "In")
                    && field(node, "comparators")
                        .as_array()
                        .is_some_and(|comparators| comparators.len() == 1)
                {
                    if let Some(extracted) = literal_str_set(&field(node, "comparators")[0]) {
                        extracted_paths.push(extracted);
                    }
                } else if ntype(node) == "Call"
                    && ntype(field(node, "func")) == "Attribute"
                    && is_name(field(field(node, "func"), "value"), "path")
                    && field(field(node, "func"), "attr").as_str() == Some("startswith")
                    && field(node, "args")
                        .as_array()
                        .is_some_and(|args| args.len() == 1)
                    && field(node, "keywords")
                        .as_array()
                        .is_some_and(|keywords| keywords.is_empty())
                {
                    let arg = &field(node, "args")[0];
                    if let Some(text) = const_str(arg) {
                        prefixes.insert(text.to_string());
                    } else if let Some(extracted) = literal_str_set(arg) {
                        prefixes.extend(extracted);
                    }
                }
            });
            // Python assigns the first extracted path set only while
            // the policy slot is still empty, in walk order.
            for extracted in extracted_paths {
                if policy.v3_protected_paths.is_none() {
                    policy.v3_protected_paths = Some(extracted);
                    break;
                }
            }
            policy.v3_protected_prefixes = prefixes;
        }
    }
    Ok(policy)
}

fn describe_set_drift(
    label: &str,
    expected: &HashSet<String>,
    actual: Option<&HashSet<String>>,
) -> Vec<String> {
    let Some(actual) = actual else {
        return vec![format!("missing {label} declaration")];
    };
    if actual == expected {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut missing: Vec<&String> = expected.difference(actual).collect();
    missing.sort();
    let mut extra: Vec<&String> = actual.difference(expected).collect();
    extra.sort();
    if !missing.is_empty() {
        let joined = missing
            .iter()
            .take(8)
            .map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "missing {label}: {joined}{}",
            if missing.len() > 8 { " ..." } else { "" }
        ));
    }
    if !extra.is_empty() {
        let joined = extra
            .iter()
            .take(8)
            .map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "extra {label}: {joined}{}",
            if extra.len() > 8 { " ..." } else { "" }
        ));
    }
    parts
}

fn str_set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// Fail fast when installed verifier policy disagrees with the
/// runner contract (the source form of
/// verify_verifier_policy_parity).
pub fn verify_source_policy_parity(
    source: &[u8],
    profile: &str,
    port: &dyn VerifierAstPort,
) -> Result<(), String> {
    let contract = CiGateContractFactory::new(profile)?.create();
    let policy = extract_verifier_policy(source, port)?;
    let mut mismatches: Vec<String> = Vec::new();
    if contract.profile == LEGACY_PROFILE && policy.checks != Some(legacy_checks()) {
        mismatches.push("legacy CHECKS differ".to_string());
    }
    if contract.profile == SCOPED_PROFILE || contract.profile == EVENT_PROFILE {
        mismatches.extend(describe_set_drift(
            "V2 protected paths",
            &str_set(&V2_PROTECTED_PATHS),
            policy.v2_protected_paths.as_ref(),
        ));
    }
    if contract.profile == SCOPED_PROFILE {
        if policy.scoped_branch_checks != Some(scoped_branch_checks()) {
            mismatches.push("SCOPED_BRANCH_CHECKS differ".to_string());
        }
        if policy.scoped_main_checks != Some(scoped_main_checks()) {
            mismatches.push("SCOPED_MAIN_CHECKS differ".to_string());
        }
    }
    if contract.profile == EVENT_PROFILE {
        mismatches.extend(describe_set_drift(
            "V3 protected paths",
            &str_set(&V3_PROTECTED_PATHS),
            policy.v3_protected_paths.as_ref(),
        ));
        mismatches.extend(describe_set_drift(
            "V3 protected prefixes",
            &str_set(&V3_PROTECTED_PREFIXES),
            Some(&policy.v3_protected_prefixes),
        ));
        let expected: Vec<(String, String, String, bool)> = FEATURE_VARIANTS
            .iter()
            .map(|(a, b, c, d)| (a.to_string(), b.to_string(), c.to_string(), *d))
            .collect();
        if policy.feature_variants != Some(expected) {
            mismatches.push("v3 FEATURE_VARIANTS differ".to_string());
        }
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Runner/verifier policy skew for {}: {}. Re-run deploy/mission-control/jenkins/setup-ci.py --native-only --execute --verification-profile {}.",
            contract.profile,
            mismatches.join("; "),
            contract.profile
        ))
    }
}

// ---------------------------------------------------------------------------
// _verify_contract_policy_parity

fn variants_pval() -> PVal {
    PVal::Tuple(
        FEATURE_VARIANTS
            .iter()
            .map(|(a, b, c, d)| {
                PVal::Tuple(vec![
                    PVal::Str(a.to_string()),
                    PVal::Str(b.to_string()),
                    PVal::Str(c.to_string()),
                    PVal::Bool(*d),
                ])
            })
            .collect(),
    )
}

fn is_event_profile_guard(node: &Value) -> bool {
    if ntype(node) != "Compare"
        || !field(node, "ops")
            .as_array()
            .is_some_and(|ops| ops.len() == 1 && ntype(&ops[0]) == "Eq")
        || !field(node, "comparators")
            .as_array()
            .is_some_and(|comparators| comparators.len() == 1)
    {
        return false;
    }
    let left = field(node, "left");
    let right = &field(node, "comparators")[0];
    let is_event =
        |side: &Value| is_name(side, "EVENT_PROFILE") || const_str(side) == Some(EVENT_PROFILE);
    (is_name(left, "profile") && is_event(right)) || (is_name(right, "profile") && is_event(left))
}

fn extract_event_gate_policy(
    gate_fn: &Value,
    constants: &HashMap<String, PVal>,
) -> Option<(HashSet<String>, HashSet<String>)> {
    let mut paths: HashSet<String> = HashSet::new();
    let mut prefixes: HashSet<String> = HashSet::new();
    let mut found = false;
    let mut boolops: Vec<Value> = Vec::new();
    walk_nodes(gate_fn, &mut |node| {
        if ntype(node) == "BoolOp" && ntype(field(node, "op")) == "And" {
            boolops.push(node.clone());
        }
    });
    for boolop in &boolops {
        let has_guard = field(boolop, "values").as_array().is_some_and(|values| {
            values.iter().any(|value| {
                let mut guard = false;
                walk_nodes(value, &mut |part| {
                    if is_event_profile_guard(part) {
                        guard = true;
                    }
                });
                guard
            })
        });
        if !has_guard {
            continue;
        }
        found = true;
        let mut compares: Vec<Value> = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        walk_nodes(boolop, &mut |node| {
            if ntype(node) == "Compare" {
                compares.push(node.clone());
            } else if ntype(node) == "Call" {
                calls.push(node.clone());
            }
        });
        for compare in &compares {
            if !is_name(field(compare, "left"), "path")
                || !field(compare, "ops")
                    .as_array()
                    .is_some_and(|ops| ops.len() == 1 && ntype(&ops[0]) == "In")
                || !field(compare, "comparators")
                    .as_array()
                    .is_some_and(|comparators| comparators.len() == 1)
            {
                continue;
            }
            let rhs = &field(compare, "comparators")[0];
            let resolved = if is_name(rhs, "V3_PROTECTED_PATHS") {
                constants.get("V3_PROTECTED_PATHS").and_then(pval_str_set)
            } else {
                eval_pure_constant(rhs)
                    .ok()
                    .and_then(|value| pval_str_set(&value))
            };
            paths.extend(resolved?);
        }
        for call in &calls {
            if ntype(field(call, "func")) == "Attribute"
                && is_name(field(field(call, "func"), "value"), "path")
                && field(field(call, "func"), "attr").as_str() == Some("startswith")
                && field(call, "args")
                    .as_array()
                    .is_some_and(|args| args.len() == 1)
            {
                if let Some(text) = const_str(&field(call, "args")[0]) {
                    prefixes.insert(text.to_string());
                }
            }
        }
    }
    if found {
        Some((paths, prefixes))
    } else {
        None
    }
}

fn verify_ci_phase_class(class_node: Option<&Value>) -> Result<(), String> {
    let mismatch = || MISMATCH.to_string();
    let class_node = class_node.ok_or_else(mismatch)?;
    if ntype(class_node) != "ClassDef"
        || !field(class_node, "bases")
            .as_array()
            .is_some_and(|bases| bases.is_empty())
        || !field(class_node, "keywords")
            .as_array()
            .is_some_and(|keywords| keywords.is_empty())
    {
        return Err(mismatch());
    }
    let decorators = field(class_node, "decorator_list")
        .as_array()
        .ok_or_else(mismatch)?;
    if decorators.len() != 1 {
        return Err(mismatch());
    }
    let decorator = &decorators[0];
    if ntype(decorator) != "Call"
        || !is_name(field(decorator, "func"), "dataclass")
        || !field(decorator, "args")
            .as_array()
            .is_some_and(|args| args.is_empty())
    {
        return Err(mismatch());
    }
    let keywords = field(decorator, "keywords")
        .as_array()
        .ok_or_else(mismatch)?;
    if keywords.len() != 1
        || field(&keywords[0], "arg").as_str() != Some("frozen")
        || !(ntype(field(&keywords[0], "value")) == "Constant"
            && field(field(&keywords[0], "value"), "value") == &Value::Bool(true))
    {
        return Err(mismatch());
    }
    let body = field(class_node, "body").as_array().ok_or_else(mismatch)?;
    let mut fields: Vec<(String, bool, Option<PVal>)> = Vec::new();
    for (index, statement) in body.iter().enumerate() {
        if index == 0
            && ntype(statement) == "Expr"
            && const_str(field(statement, "value")).is_some()
        {
            continue;
        }
        if ntype(statement) != "AnnAssign"
            || ntype(field(statement, "target")) != "Name"
            || field(statement, "simple").as_i64() != Some(1)
        {
            return Err(mismatch());
        }
        let name = field(field(statement, "target"), "id")
            .as_str()
            .ok_or_else(mismatch)?
            .to_string();
        let value = field(statement, "value");
        if value.is_null() {
            fields.push((name, false, None));
        } else {
            let parsed = eval_pure_constant(value).map_err(|_| mismatch())?;
            fields.push((name, true, Some(parsed)));
        }
    }
    let expected: Vec<(String, bool, Option<PVal>)> = vec![
        ("phase_id".to_string(), false, None),
        ("kind".to_string(), false, None),
        ("argv".to_string(), false, None),
        (
            "logical_gate".to_string(),
            true,
            Some(PVal::Str("check".to_string())),
        ),
    ];
    if fields == expected {
        Ok(())
    } else {
        Err(mismatch())
    }
}

fn verify_event_checks_dispatch(function_node: Option<&Value>) -> Result<(), String> {
    let mismatch = || MISMATCH.to_string();
    let function_node = function_node.ok_or_else(mismatch)?;
    let mut ifs: Vec<Value> = Vec::new();
    walk_nodes(function_node, &mut |node| {
        if ntype(node) == "If" {
            ifs.push(node.clone());
        }
    });
    for statement in &ifs {
        let mut guarded = false;
        walk_nodes(field(statement, "test"), &mut |part| {
            if is_event_profile_guard(part) {
                guarded = true;
            }
        });
        if !guarded {
            continue;
        }
        let mut calls_event = false;
        walk_nodes(statement, &mut |part| {
            if ntype(part) == "Call" && is_name(field(part, "func"), "event_phases_for_scope") {
                calls_event = true;
            }
        });
        if calls_event {
            return Ok(());
        }
    }
    Err(mismatch())
}

// ---------------------------------------------------------------------------
// The bounded static evaluator for event_phases_for_scope

#[derive(Debug, Clone, PartialEq)]
enum EVal {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    None,
    Tuple(Vec<EVal>),
    List(Vec<EVal>),
    Set(Vec<EVal>),
    Phase(CiPhase),
}

fn eval_from_pval(value: &PVal) -> EVal {
    match value {
        PVal::Str(text) => EVal::Str(text.clone()),
        PVal::Int(int) => EVal::Int(*int),
        PVal::Float(float) => EVal::Float(*float),
        PVal::Bool(flag) => EVal::Bool(*flag),
        PVal::None => EVal::None,
        PVal::Tuple(items) => EVal::Tuple(items.iter().map(eval_from_pval).collect()),
        PVal::List(items) => EVal::List(items.iter().map(eval_from_pval).collect()),
        PVal::Set(items) => EVal::Set(items.iter().map(eval_from_pval).collect()),
    }
}

fn build_ci_phase(positional: Vec<EVal>, keywords: Vec<(String, EVal)>) -> Result<CiPhase, String> {
    const NAMES: [&str; 4] = ["phase_id", "kind", "argv", "logical_gate"];
    if positional.len() > NAMES.len() {
        return Err("Invalid CiPhase arguments in verifier policy".to_string());
    }
    let mut values: HashMap<String, EVal> = HashMap::new();
    for (name, value) in NAMES.iter().zip(positional) {
        values.insert(name.to_string(), value);
    }
    for (name, value) in keywords {
        if !NAMES.contains(&name.as_str()) || values.contains_key(&name) {
            return Err("Invalid CiPhase keyword argument in verifier policy".to_string());
        }
        values.insert(name, value);
    }
    values
        .entry("logical_gate".to_string())
        .or_insert_with(|| EVal::Str("check".to_string()));
    for name in NAMES {
        if !values.contains_key(name) {
            return Err("Missing CiPhase field in verifier policy".to_string());
        }
    }
    let invalid = || "Invalid CiPhase field in verifier policy".to_string();
    let phase_id = match values.remove("phase_id") {
        Some(EVal::Str(text)) => text,
        _ => return Err(invalid()),
    };
    let kind = match values.remove("kind") {
        Some(EVal::Str(text)) => text,
        _ => return Err(invalid()),
    };
    let argv = match values.remove("argv") {
        Some(EVal::Tuple(items)) => {
            let mut argv = Vec::new();
            for item in items {
                match item {
                    EVal::Str(text) => argv.push(text),
                    _ => return Err(invalid()),
                }
            }
            argv
        }
        _ => return Err(invalid()),
    };
    let logical_gate = match values.remove("logical_gate") {
        Some(EVal::Str(text)) => text,
        _ => return Err(invalid()),
    };
    Ok(CiPhase {
        phase_id,
        kind,
        argv,
        logical_gate,
    })
}

struct PhaseEvaluator<'a> {
    constants: &'a HashMap<String, PVal>,
    branch_scope: &'a str,
    param: String,
    env: HashMap<String, EVal>,
    steps: usize,
}

impl PhaseEvaluator<'_> {
    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > 4096 {
            return Err("Verifier event phase evaluation exceeded its step budget".to_string());
        }
        Ok(())
    }

    fn checked_collection(items: Vec<EVal>) -> Result<Vec<EVal>, String> {
        if items.len() > 256 {
            return Err("Verifier event phase collection exceeded its bound".to_string());
        }
        Ok(items)
    }

    fn lookup(&self, name: &str) -> Result<EVal, String> {
        if name == self.param {
            return Err(
                "Unsupported direct use of branch_scope in verifier event_phases_for_scope"
                    .to_string(),
            );
        }
        if let Some(value) = self.env.get(name) {
            return Ok(value.clone());
        }
        if let Some(value) = self.constants.get(name) {
            return Ok(eval_from_pval(value));
        }
        Err("Unknown symbol in verifier event_phases_for_scope".to_string())
    }

    fn bind(&mut self, name: &str, value: EVal) -> Result<(), String> {
        if name == self.param || name == "CiPhase" || name == "tuple" {
            return Err("Illegal assignment target in verifier event_phases_for_scope".to_string());
        }
        self.env.insert(name.to_string(), value);
        Ok(())
    }

    fn truth(value: EVal) -> Result<bool, String> {
        match value {
            EVal::Bool(flag) => Ok(flag),
            _ => Err("Unsupported truth value in verifier event_phases_for_scope".to_string()),
        }
    }

    fn eval_expr(&mut self, node: &Value) -> Result<EVal, String> {
        self.tick()?;
        match ntype(node) {
            "Constant" => {
                let value = field(node, "value");
                match value {
                    Value::String(text) => Ok(EVal::Str(text.clone())),
                    Value::Bool(flag) => Ok(EVal::Bool(*flag)),
                    Value::Null => Ok(EVal::None),
                    Value::Number(number) => {
                        match number.as_i64() {
                            Some(int) => Ok(EVal::Int(int)),
                            None => Err("Unsupported constant in verifier event_phases_for_scope"
                                .to_string()),
                        }
                    }
                    _ => Err("Unsupported constant in verifier event_phases_for_scope".to_string()),
                }
            }
            "Name" => self.lookup(field(node, "id").as_str().unwrap_or_default()),
            "List" | "Tuple" => {
                let mut items = Vec::new();
                for element in field(node, "elts").as_array().cloned().unwrap_or_default() {
                    if ntype(&element) == "Starred" {
                        match self.eval_expr(field(&element, "value"))? {
                            EVal::Tuple(values) | EVal::List(values) => items.extend(values),
                            _ => {
                                return Err(
                                    "Unsupported starred value in verifier event_phases_for_scope"
                                        .to_string(),
                                )
                            }
                        }
                    } else {
                        items.push(self.eval_expr(&element)?);
                    }
                    Self::checked_collection(items.clone())?;
                }
                let items = Self::checked_collection(items)?;
                Ok(if ntype(node) == "List" {
                    EVal::List(items)
                } else {
                    EVal::Tuple(items)
                })
            }
            "BinOp" => {
                if ntype(field(node, "op")) != "Add" {
                    return Err(
                        "Unsupported binary operation in verifier event_phases_for_scope"
                            .to_string(),
                    );
                }
                let left = self.eval_expr(field(node, "left"))?;
                let right = self.eval_expr(field(node, "right"))?;
                match (left, right) {
                    (EVal::Str(a), EVal::Str(b)) => {
                        let joined = format!("{a}{b}");
                        if joined.len() > 4096 {
                            return Err(
                                "Verifier string concatenation exceeded its bound".to_string()
                            );
                        }
                        Ok(EVal::Str(joined))
                    }
                    (EVal::Tuple(mut a), EVal::Tuple(b)) => {
                        a.extend(b);
                        Ok(EVal::Tuple(Self::checked_collection(a)?))
                    }
                    (EVal::List(mut a), EVal::List(b)) => {
                        a.extend(b);
                        Ok(EVal::List(Self::checked_collection(a)?))
                    }
                    _ => Err(
                        "Unsupported binary operands in verifier event_phases_for_scope"
                            .to_string(),
                    ),
                }
            }
            "UnaryOp" => {
                if ntype(field(node, "op")) != "Not" {
                    return Err(
                        "Unsupported unary operation in verifier event_phases_for_scope"
                            .to_string(),
                    );
                }
                let operand = self.eval_expr(field(node, "operand"))?;
                Ok(EVal::Bool(!Self::truth(operand)?))
            }
            "BoolOp" => {
                let is_and = ntype(field(node, "op")) == "And";
                for value_node in field(node, "values")
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    let value = Self::truth(self.eval_expr(&value_node)?)?;
                    if is_and && !value {
                        return Ok(EVal::Bool(false));
                    }
                    if !is_and && value {
                        return Ok(EVal::Bool(true));
                    }
                }
                Ok(EVal::Bool(is_and))
            }
            "Compare" => self.eval_compare(node),
            "IfExp" => {
                let test = self.eval_expr(field(node, "test"))?;
                if Self::truth(test)? {
                    self.eval_expr(field(node, "body"))
                } else {
                    self.eval_expr(field(node, "orelse"))
                }
            }
            "Call" => self.eval_call(node),
            _ => Err("Unsupported expression in verifier event_phases_for_scope".to_string()),
        }
    }

    fn eval_compare(&mut self, node: &Value) -> Result<EVal, String> {
        let ops = field(node, "ops").as_array().cloned().unwrap_or_default();
        let comparators = field(node, "comparators")
            .as_array()
            .cloned()
            .unwrap_or_default();
        if ops.len() != 1 || comparators.len() != 1 {
            return Err("Unsupported comparison in verifier event_phases_for_scope".to_string());
        }
        let op = ntype(&ops[0]).to_string();
        let left_node = field(node, "left").clone();
        let right_node = comparators[0].clone();
        let is_param =
            |side: &Value| ntype(side) == "Name" && field(side, "id").as_str() == Some(&self.param);
        if is_param(&left_node) || is_param(&right_node) {
            if is_param(&left_node) && is_param(&right_node) {
                return Err(
                    "Unsupported branch_scope comparison in verifier event_phases_for_scope"
                        .to_string(),
                );
            }
            if op != "Eq" && op != "NotEq" {
                return Err(
                    "Unsupported branch_scope comparison in verifier event_phases_for_scope"
                        .to_string(),
                );
            }
            let other = if is_param(&left_node) {
                self.eval_expr(&right_node)?
            } else {
                self.eval_expr(&left_node)?
            };
            if other != EVal::Str("main".to_string()) {
                return Err(
                    "Unsupported branch_scope target in verifier event_phases_for_scope"
                        .to_string(),
                );
            }
            let equal = self.branch_scope == "main";
            return Ok(EVal::Bool(if op == "Eq" { equal } else { !equal }));
        }
        let left = self.eval_expr(&left_node)?;
        let right = self.eval_expr(&right_node)?;
        match op.as_str() {
            "Eq" => Ok(EVal::Bool(left == right)),
            "NotEq" => Ok(EVal::Bool(left != right)),
            "In" | "NotIn" => {
                let found = match (&left, &right) {
                    (EVal::Str(needle), EVal::Str(haystack)) => haystack.contains(needle.as_str()),
                    (_, EVal::Tuple(items)) | (_, EVal::List(items)) | (_, EVal::Set(items)) => {
                        items.contains(&left)
                    }
                    _ => {
                        return Err(
                            "Unsupported membership test in verifier event_phases_for_scope"
                                .to_string(),
                        )
                    }
                };
                Ok(EVal::Bool(if op == "In" { found } else { !found }))
            }
            _ => Err("Unsupported comparison in verifier event_phases_for_scope".to_string()),
        }
    }

    fn eval_call(&mut self, node: &Value) -> Result<EVal, String> {
        if ntype(field(node, "func")) != "Name" {
            return Err("Unsupported call in verifier event_phases_for_scope".to_string());
        }
        let name = field(field(node, "func"), "id")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let args = field(node, "args").as_array().cloned().unwrap_or_default();
        let keywords = field(node, "keywords")
            .as_array()
            .cloned()
            .unwrap_or_default();
        match name.as_str() {
            "CiPhase" => {
                let mut positional = Vec::new();
                for arg in &args {
                    if ntype(arg) == "Starred" {
                        return Err(
                            "Unsupported CiPhase argument unpacking in verifier policy".to_string()
                        );
                    }
                    positional.push(self.eval_expr(arg)?);
                }
                let mut named = Vec::new();
                for keyword in &keywords {
                    let Some(arg_name) = field(keyword, "arg").as_str() else {
                        return Err(
                            "Unsupported CiPhase keyword unpacking in verifier policy".to_string()
                        );
                    };
                    let value = self.eval_expr(field(keyword, "value"))?;
                    named.push((arg_name.to_string(), value));
                }
                Ok(EVal::Phase(build_ci_phase(positional, named)?))
            }
            "tuple" => {
                if args.len() != 1 || !keywords.is_empty() || ntype(&args[0]) == "Starred" {
                    return Err(
                        "Unsupported tuple call in verifier event_phases_for_scope".to_string()
                    );
                }
                match self.eval_expr(&args[0])? {
                    EVal::List(items) => Ok(EVal::Tuple(items)),
                    value @ EVal::Tuple(_) => Ok(value),
                    _ => Err(
                        "Unsupported tuple argument in verifier event_phases_for_scope".to_string(),
                    ),
                }
            }
            _ => Err("Unsupported call in verifier event_phases_for_scope".to_string()),
        }
    }

    fn exec_block(
        &mut self,
        statements: &[Value],
        allow_docstring: bool,
    ) -> Result<Option<EVal>, String> {
        for (index, statement) in statements.iter().enumerate() {
            self.tick()?;
            if index == 0
                && allow_docstring
                && ntype(statement) == "Expr"
                && const_str(field(statement, "value")).is_some()
            {
                continue;
            }
            match ntype(statement) {
                "Assign" => {
                    let targets = field(statement, "targets")
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    if targets.len() != 1 || ntype(&targets[0]) != "Name" {
                        return Err(
                            "Unsupported assignment in verifier event_phases_for_scope".to_string()
                        );
                    }
                    let value = self.eval_expr(field(statement, "value"))?;
                    let name = field(&targets[0], "id")
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    self.bind(&name, value)?;
                }
                "Expr" => {
                    let call = field(statement, "value");
                    let valid = ntype(call) == "Call"
                        && ntype(field(call, "func")) == "Attribute"
                        && ntype(field(field(call, "func"), "value")) == "Name"
                        && matches!(
                            field(field(call, "func"), "attr").as_str(),
                            Some("append") | Some("extend")
                        )
                        && field(call, "args")
                            .as_array()
                            .is_some_and(|args| args.len() == 1)
                        && field(call, "keywords")
                            .as_array()
                            .is_some_and(|keywords| keywords.is_empty())
                        && ntype(&field(call, "args")[0]) != "Starred";
                    if !valid {
                        return Err(
                            "Unsupported expression statement in verifier event_phases_for_scope"
                                .to_string(),
                        );
                    }
                    let target_name = field(field(field(call, "func"), "value"), "id")
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let attr = field(field(call, "func"), "attr")
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let argument = self.eval_expr(&field(call, "args")[0])?;
                    let target = self.env.get_mut(&target_name).ok_or_else(|| {
                        "Unsupported list mutation in verifier event_phases_for_scope".to_string()
                    })?;
                    let EVal::List(items) = target else {
                        return Err(
                            "Unsupported list mutation in verifier event_phases_for_scope"
                                .to_string(),
                        );
                    };
                    if attr == "append" {
                        items.push(argument);
                    } else {
                        match argument {
                            EVal::Tuple(values) | EVal::List(values) => items.extend(values),
                            _ => {
                                return Err(
                                    "Unsupported list mutation in verifier event_phases_for_scope"
                                        .to_string(),
                                )
                            }
                        }
                    }
                    Self::checked_collection(items.clone())?;
                }
                "For" => {
                    if !field(statement, "orelse")
                        .as_array()
                        .is_some_and(|orelse| orelse.is_empty())
                    {
                        return Err(
                            "Unsupported for statement in verifier event_phases_for_scope"
                                .to_string(),
                        );
                    }
                    let iterable = match self.eval_expr(field(statement, "iter"))? {
                        EVal::Tuple(items) | EVal::List(items) => items,
                        _ => {
                            return Err("Unsupported iterable in verifier event_phases_for_scope"
                                .to_string())
                        }
                    };
                    if iterable.len() > 256 {
                        return Err(
                            "Verifier event phase collection exceeded its bound".to_string()
                        );
                    }
                    let target = field(statement, "target").clone();
                    let body = field(statement, "body")
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    for item in iterable {
                        match ntype(&target) {
                            "Name" => {
                                let name = field(&target, "id")
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                self.bind(&name, item)?;
                            }
                            "Tuple" => {
                                let elts = field(&target, "elts")
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default();
                                let values = match item {
                                    EVal::Tuple(values) | EVal::List(values) => values,
                                    _ => {
                                        return Err(
                                            "Invalid verifier event phase unpacking".to_string()
                                        )
                                    }
                                };
                                if values.len() != elts.len()
                                    || elts.iter().any(|elt| ntype(elt) != "Name")
                                {
                                    return Err(
                                        "Invalid verifier event phase unpacking".to_string()
                                    );
                                }
                                for (elt, value) in elts.iter().zip(values) {
                                    let name =
                                        field(elt, "id").as_str().unwrap_or_default().to_string();
                                    self.bind(&name, value)?;
                                }
                            }
                            _ => {
                                return Err(
                                    "Unsupported loop target in verifier event_phases_for_scope"
                                        .to_string(),
                                )
                            }
                        }
                        if let Some(returned) = self.exec_block(&body, false)? {
                            return Ok(Some(returned));
                        }
                    }
                }
                "If" => {
                    let test = self.eval_expr(field(statement, "test"))?;
                    let branch = if Self::truth(test)? {
                        field(statement, "body")
                    } else {
                        field(statement, "orelse")
                    };
                    let branch = branch.as_array().cloned().unwrap_or_default();
                    if let Some(returned) = self.exec_block(&branch, false)? {
                        return Ok(Some(returned));
                    }
                }
                "Return" => {
                    let value = field(statement, "value");
                    if value.is_null() {
                        return Err(
                            "Unsupported return in verifier event_phases_for_scope".to_string()
                        );
                    }
                    return Ok(Some(self.eval_expr(value)?));
                }
                _ => {
                    return Err(
                        "Unsupported statement in verifier event_phases_for_scope".to_string()
                    )
                }
            }
        }
        Ok(None)
    }
}

fn evaluate_verifier_event_phases(
    function_node: &Value,
    constants: &HashMap<String, PVal>,
    branch_scope: &str,
) -> Result<Vec<CiPhase>, String> {
    if ntype(function_node) != "FunctionDef" {
        return Err(MISMATCH.to_string());
    }
    let signature_error = || "Unsupported verifier event_phases_for_scope signature".to_string();
    if !field(function_node, "decorator_list")
        .as_array()
        .is_some_and(|decorators| decorators.is_empty())
    {
        return Err(signature_error());
    }
    let args = field(function_node, "args");
    let positional = field(args, "args").as_array().cloned().unwrap_or_default();
    if !field(args, "posonlyargs")
        .as_array()
        .is_some_and(|items| items.is_empty())
        || positional.len() != 1
        || !field(args, "vararg").is_null()
        || !field(args, "kwonlyargs")
            .as_array()
            .is_some_and(|items| items.is_empty())
        || !field(args, "kwarg").is_null()
        || !field(args, "defaults")
            .as_array()
            .is_some_and(|items| items.is_empty())
    {
        return Err(signature_error());
    }
    let param = field(&positional[0], "arg")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut evaluator = PhaseEvaluator {
        constants,
        branch_scope,
        param,
        env: HashMap::new(),
        steps: 0,
    };
    let body = field(function_node, "body")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let returned = evaluator.exec_block(&body, true)?;
    match returned {
        Some(EVal::Tuple(items)) => {
            let mut phases = Vec::new();
            for item in items {
                match item {
                    EVal::Phase(phase) => phases.push(phase),
                    _ => return Err(MISMATCH.to_string()),
                }
            }
            Ok(phases)
        }
        _ => Err(MISMATCH.to_string()),
    }
}

/// The contract form of _verify_contract_policy_parity: compare the
/// verifier's declared constants and the statically evaluated event
/// plan against the active gate contract.
pub fn verify_contract_policy_parity(
    contract: &CiGateContract,
    source: &[u8],
    port: &dyn VerifierAstPort,
) -> Result<(), String> {
    if std::str::from_utf8(source).is_err() {
        return Err("Installed verifier policy is unreadable".to_string());
    }
    let tree = port
        .parse(source)
        .map_err(|_| "Installed verifier policy is unreadable".to_string())?;
    let mut constants: HashMap<String, PVal> = HashMap::new();
    let mut functions: HashMap<String, Value> = HashMap::new();
    let mut classes: HashMap<String, Value> = HashMap::new();
    for statement in field(&tree, "body").as_array().cloned().unwrap_or_default() {
        match ntype(&statement) {
            "Assign" => {
                let targets = field(&statement, "targets")
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if targets.len() == 1 && ntype(&targets[0]) == "Name" {
                    if let Ok(value) = eval_pure_constant(field(&statement, "value")) {
                        constants.insert(
                            field(&targets[0], "id")
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            value,
                        );
                    }
                }
            }
            "FunctionDef" => {
                functions.insert(
                    field(&statement, "name")
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    statement.clone(),
                );
            }
            "ClassDef" => {
                classes.insert(
                    field(&statement, "name")
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    statement.clone(),
                );
            }
            _ => {}
        }
    }
    let mismatch = || MISMATCH.to_string();
    let gate_fn = functions
        .get("profile_protected_gate_path")
        .ok_or_else(mismatch)?;
    let mut has_contract_path = false;
    walk_nodes(gate_fn, &mut |node| {
        if ntype(node) == "Constant" && const_str(node) == Some(CONTRACT_PATH) {
            has_contract_path = true;
        }
    });
    if contract.profile == LEGACY_PROFILE {
        if constants.get("CHECKS") == Some(&commands_pval(&legacy_checks())) && has_contract_path {
            return Ok(());
        }
        return Err(mismatch());
    }
    let mut references_v2 = false;
    walk_nodes(gate_fn, &mut |node| {
        if is_name(node, "V2_PROTECTED_PATHS") {
            references_v2 = true;
        }
    });
    let v2_matches = constants
        .get("V2_PROTECTED_PATHS")
        .and_then(pval_str_set)
        .is_some_and(|paths| paths == str_set(&V2_PROTECTED_PATHS));
    if !v2_matches || !references_v2 || !has_contract_path {
        return Err(mismatch());
    }
    if contract.profile == SCOPED_PROFILE {
        if constants.get("SCOPED_BRANCH_CHECKS") == Some(&commands_pval(&scoped_branch_checks()))
            && constants.get("SCOPED_MAIN_CHECKS") == Some(&commands_pval(&scoped_main_checks()))
        {
            return Ok(());
        }
        return Err(mismatch());
    }
    // EVENT profile.
    if constants.get("EVENT_PROFILE") != Some(&PVal::Str(EVENT_PROFILE.to_string())) {
        return Err(mismatch());
    }
    let all_features = PVal::Tuple(
        ALL_FEATURES
            .iter()
            .map(|item| PVal::Str(item.to_string()))
            .collect(),
    );
    if constants.get("ALL_FEATURES") != Some(&all_features) {
        return Err(mismatch());
    }
    if constants.get("FEATURE_VARIANTS") != Some(&variants_pval()) {
        return Err(mismatch());
    }
    let expected_gate = (
        str_set(&V3_PROTECTED_PATHS),
        str_set(&V3_PROTECTED_PREFIXES),
    );
    if extract_event_gate_policy(gate_fn, &constants) != Some(expected_gate) {
        return Err(mismatch());
    }
    verify_ci_phase_class(classes.get("CiPhase"))?;
    verify_event_checks_dispatch(functions.get("checks_for_scope"))?;
    let phase_fn = functions
        .get("event_phases_for_scope")
        .ok_or_else(mismatch)?;
    for scope in ["main", "detached", "branch/parity-check"] {
        let evaluated = evaluate_verifier_event_phases(phase_fn, &constants, scope)?;
        if evaluated != event_phases_for_scope(scope) {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// verify_verifier_policy_parity with a contract object: source
/// parity first, then contract parity.
pub fn verify_verifier_policy_parity_contract(
    contract: &CiGateContract,
    source: &[u8],
    port: &dyn VerifierAstPort,
) -> Result<(), String> {
    verify_source_policy_parity(source, &contract.profile, port)?;
    verify_contract_policy_parity(contract, source, port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_contract::CiGateContractFactory;

    fn verifier_source() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scripts/jenkins/verify-candidate.py");
        std::fs::read(&path).expect("the real verifier source is a parity fixture")
    }

    #[test]
    fn real_verifier_matches_all_profiles() {
        let source = verifier_source();
        let port = PythonAstPort;
        for profile in [LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE] {
            verify_source_policy_parity(&source, profile, &port)
                .unwrap_or_else(|error| panic!("source parity {profile}: {error}"));
            let contract = CiGateContractFactory::new(profile).unwrap().create();
            verify_verifier_policy_parity_contract(&contract, &source, &port)
                .unwrap_or_else(|error| panic!("contract parity {profile}: {error}"));
        }
        let policy = extract_verifier_policy(&source, &port).unwrap();
        assert_eq!(policy.v3_protected_paths.unwrap().len(), 26);
        assert_eq!(policy.v3_protected_prefixes.len(), 9);
    }

    #[test]
    fn detects_v3_protected_path_skew() {
        let source = verifier_source();
        let text = String::from_utf8(source).unwrap();
        let skewed = text
            .replace(
                "            'deploy/mission-control/agent-runner/work_external_plan_override.py',\n",
                "",
            )
            .replace(
                "            'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql',\n",
                "",
            );
        assert_ne!(skewed, text);
        let port = PythonAstPort;
        let error =
            verify_source_policy_parity(skewed.as_bytes(), EVENT_PROFILE, &port).unwrap_err();
        assert!(error.contains("work_external_plan_override.py"), "{error}");
        assert!(
            error.contains("0038_external_plan_approve_override.sql"),
            "{error}"
        );
        assert!(error.contains(
            "setup-ci.py --native-only --execute --verification-profile forge-sdlc-jenkins-v3"
        ));
        // v2 policy is unaffected by v3-only protected paths.
        verify_source_policy_parity(skewed.as_bytes(), SCOPED_PROFILE, &port).unwrap();
    }

    #[test]
    fn detects_legacy_checks_skew_and_unparseable_source() {
        let source = verifier_source();
        let text = String::from_utf8(source).unwrap();
        let skewed = text.replace("('/usr/bin/make', 'ci'),", "('/usr/bin/make', 'check'),");
        assert_ne!(skewed, text);
        let port = PythonAstPort;
        let error =
            verify_source_policy_parity(skewed.as_bytes(), LEGACY_PROFILE, &port).unwrap_err();
        assert!(error.contains("legacy CHECKS differ"), "{error}");
        assert!(verify_source_policy_parity(b"def broken(:\n", LEGACY_PROFILE, &port).is_err());
    }
}
