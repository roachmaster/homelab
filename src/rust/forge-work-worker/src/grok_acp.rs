//! Selected ACP-v1 transport and deny-by-default native permission
//! broker (work_grok_acp.py).
//!
//! No subprocesses, credentials, session discovery or network effects
//! live here. The existing supervisor owns the process, OS sandbox
//! and credential lifetime.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::cli_argv::{CHILD_STATE, CHILD_WORKSPACE};
use crate::cli_transport::parse_record;
use crate::provider_events::GrokEvents;
use crate::quota::quota_diagnostic;

pub const WORKSPACE: &str = CHILD_WORKSPACE;
pub const STATE: &str = CHILD_STATE;
pub const SELECTION: &str = "acp-constrained-v1";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_TOTAL: usize = 32 * 1024 * 1024;
// Bare named ask rules are documented to request approval even for
// normally automatic local reads and built-in read-only shell
// handling. No allow rules.
pub const CONFIG_SUFFIX: &str = "\n[permission]\nask = [\"Bash\", \"Read\", \"Edit\", \"Grep\", \"Glob\", \"run_terminal_command\", \"read_file\", \"list_dir\", \"grep\", \"search_replace\", \"write\"]\ndeny = [\"MCPTool(*)\"]\n";

pub const DIRECTION: &str = "Use native read_file/grep/list_dir in the workspace, closed read-only git commands, or the exact report_progress.py helper. Implementation alone permits workspace edits and bounded build/test commands. Use plain report text. Do not use shell substitutions, glob/brace/tilde expansion, redirections, publishing, network or external tools.";

const STOP_REASONS: [&str; 5] = [
    "end_turn",
    "cancelled",
    "max_tokens",
    "max_turn_requests",
    "refusal",
];

pub fn wire(value: &Value) -> Vec<u8> {
    let mut data = serde_json::to_vec(value).unwrap_or_default();
    data.push(b'\n');
    data
}

fn counter(value: &Value) -> Option<u128> {
    // Python `type(v) is int` plus 0 <= v <= MAX_COUNTER (10**40 - 1,
    // forty digits; any u64 is inside it, floats and bools are not
    // ints).
    value.as_u64().map(|v| v as u128)
}

/// ACP inputTokens includes cache; emit the existing disjoint end
/// buckets (work_grok_acp.terminal_usage). Missing buckets stay
/// unknown; never subtract inferred zeros or sum step counters.
pub fn terminal_usage(value: Option<&Value>) -> Result<Option<Value>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let map = value
        .as_object()
        .ok_or("Invalid ACP terminal usage")?;
    let field = |key: &str| -> Result<Option<u128>, String> {
        match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(other) => counter(other).ok_or("Invalid ACP terminal counter".to_string()).map(Some),
        }
    };
    let full = field("inputTokens")?;
    let output = field("outputTokens")?;
    let cached = field("cachedReadTokens")?;
    let created = field("cacheCreationTokens")?;
    let total = field("totalTokens")?;
    let uncached = match (full, cached, created) {
        (Some(full), Some(cached), Some(created)) => {
            let value = full as i128 - cached as i128 - created as i128;
            if value < 0 {
                return Err("Invalid ACP cached input".to_string());
            }
            Some(value as u128)
        }
        _ => None,
    };
    if let (Some(full), Some(output), Some(total)) = (full, output, total) {
        if full + output != total {
            return Err("Inconsistent ACP terminal accounting".to_string());
        }
    }
    let number = |value: Option<u128>| match value {
        Some(value) => Value::from(value as u64),
        None => Value::Null,
    };
    let mut buckets = Map::new();
    buckets.insert("input_tokens".to_string(), number(uncached));
    buckets.insert("output_tokens".to_string(), number(output));
    buckets.insert("cache_read_input_tokens".to_string(), number(cached));
    buckets.insert("cache_creation_input_tokens".to_string(), number(created));
    buckets.insert("total_tokens".to_string(), number(total));
    GrokEvents::usage(Some(&Value::Object(buckets)))
}

/// The ACP argv (work_grok_acp.argv): ask every permission through
/// ACP; no Bash/Read/Edit allow rules, yolo, dontAsk, leader, managed
/// MCP or client terminal/filesystem delegation.
pub fn grok_acp_argv(binary: &Path, model: &str, implement: bool) -> Vec<String> {
    let mut tools = "read_file,grep,list_dir,run_terminal_cmd".to_string();
    if implement {
        tools += ",search_replace,write";
    }
    [
        binary.to_string_lossy().into_owned(),
        "--no-auto-update".to_string(),
        "--trust".to_string(),
        "--cwd".to_string(),
        WORKSPACE.to_string(),
        "--no-memory".to_string(),
        "--no-subagents".to_string(),
        "--no-plan".to_string(),
        "--disable-web-search".to_string(),
        "--permission-mode".to_string(),
        "default".to_string(),
        "--tools".to_string(),
        tools,
        "--deny".to_string(),
        "MCPTool(*)".to_string(),
        "--sandbox".to_string(),
        if implement { "workspace" } else { "read-only" }.to_string(),
        "agent".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--no-leader".to_string(),
        "stdio".to_string(),
    ]
    .into_iter()
    .collect()
}

/// The git config paths bound for an acquired isolated worktree
/// (work_grok_acp.git_config_paths). The workspace provider has
/// already proved the git directory belongs to the approved common
/// repository; derive from that proof for both bare feature and
/// legacy .git repositories.
pub fn git_config_paths(git_directory: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let parent = git_directory
        .parent()
        .ok_or("ACP requires an acquired isolated worktree")?;
    if parent.file_name().and_then(|n| n.to_str()) != Some("worktrees") {
        return Err("ACP requires an acquired isolated worktree".to_string());
    }
    let common = parent
        .parent()
        .ok_or("ACP requires an acquired isolated worktree")?;
    Ok((
        common.join("config"),
        git_directory.join("config.worktree"),
        PathBuf::from("/etc/gitconfig"),
    ))
}

fn charset(value: &str, min: usize, max: usize, allow: fn(char) -> bool) -> bool {
    let count = value.chars().count();
    count >= min && count <= max && value.chars().all(allow)
}

fn is_bool_value(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "false" | "0" | "1")
}

/// The fetch refspec spelling:
/// `\+?refs/(heads/)?\*:refs/(remotes/<name>/)?\*`.
fn harmless_fetch(value: &str) -> bool {
    let value = value.strip_prefix('+').unwrap_or(value);
    let Some(value) = value.strip_prefix("refs/") else {
        return false;
    };
    let value = value.strip_prefix("heads/").unwrap_or(value);
    let Some(value) = value.strip_prefix("*:refs/") else {
        return false;
    };
    let value = match value.strip_prefix("remotes/") {
        Some(rest) => {
            let Some((name, tail)) = rest.split_once('/') else {
                return false;
            };
            if !charset(name, 1, usize::MAX, |c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
            }) {
                return false;
            }
            tail
        }
        None => value,
    };
    value == "*"
}

/// Closed Git config subset, not a growing list of executable-key
/// guesses (work_grok_acp.harmless_git_config). Ordinary repository
/// identity is allowed. Includes, filters, aliases, signature
/// programs, attribute paths, pagers and unknown syntax fail shut.
/// Inline section+key is valid Git syntax and is parsed explicitly.
pub fn harmless_git_config(content: &str) -> bool {
    if content.contains('\\')
        || content
            .chars()
            .any(|c| (c as u32) < 32 && !matches!(c, '\n' | '\r' | '\t'))
    {
        return false;
    }
    let mut section: Option<String> = None;
    for raw_line in content.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            let Some((name, subsection, rest)) = parse_section_header(line) else {
                return false;
            };
            if matches!(name.as_str(), "remote" | "branch") != subsection.is_some() {
                return false;
            }
            section = Some(name);
            line = rest;
            if line.is_empty() {
                continue;
            }
        }
        let Some((key, value)) = parse_assignment(line) else {
            return false;
        };
        let Some(current) = section.as_deref() else {
            return false;
        };
        let key = key.to_ascii_lowercase();
        let value = value.as_str();
        let accepted = match current {
            "core" => match key.as_str() {
                "repositoryformatversion" => matches!(value, "0" | "1"),
                "filemode" | "bare" | "logallrefupdates" | "ignorecase" | "precomposeunicode"
                | "symlinks" => is_bool_value(value),
                "fsmonitor" => matches!(value.to_ascii_lowercase().as_str(), "false" | "0"),
                "hookspath" => value == "/dev/null",
                _ => false,
            },
            "extensions" => match key.as_str() {
                "worktreeconfig" => is_bool_value(value),
                "objectformat" => value == "sha1",
                _ => false,
            },
            "remote" => match key.as_str() {
                "url" => charset(value, 1, 4096, |c| {
                    c.is_ascii_alphanumeric() || matches!(c, '@' | ':' | '/' | '.' | '_' | '~' | '+' | '-')
                }),
                "fetch" => harmless_fetch(value),
                "mirror" => is_bool_value(value),
                _ => false,
            },
            "branch" => match key.as_str() {
                "remote" => charset(value, 1, 128, |c| {
                    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
                }),
                "merge" => value
                    .strip_prefix("refs/heads/")
                    .map(|rest| {
                        charset(rest, 1, 128, |c| {
                            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')
                        })
                    })
                    .unwrap_or(false),
                _ => false,
            },
            _ => false,
        };
        if !accepted {
            return false;
        }
    }
    true
}

/// `\[\s*(core|extensions|remote|branch)(?:\s+"([A-Za-z0-9_./-]+)")?\s*\]`
/// (case-insensitive section name); returns the lowercased name, the
/// subsection, and the unparsed remainder of the line.
fn parse_section_header(line: &str) -> Option<(String, Option<String>, &str)> {
    let inner = line.strip_prefix('[')?;
    let inner = inner.trim_start();
    let name_end = inner
        .find(|c: char| !(c.is_ascii_alphanumeric()))
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    if !matches!(name.as_str(), "core" | "extensions" | "remote" | "branch") {
        return None;
    }
    let mut rest = inner[name_end..].trim_start();
    let mut subsection = None;
    if let Some(quoted) = rest.strip_prefix('"') {
        let close = quoted.find('"')?;
        let value = &quoted[..close];
        if !charset(value, 1, usize::MAX, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')
        }) {
            return None;
        }
        subsection = Some(value.to_string());
        rest = quoted[close + 1..].trim_start();
    }
    let rest = rest.strip_prefix(']')?;
    Some((name, subsection, rest.trim()))
}

/// `([A-Za-z][A-Za-z0-9-]*)\s*=\s*(.*?)\s*` over the whole line.
fn parse_assignment(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let mut chars = key.chars();
    if !chars.next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

/// Type-strict JSON equality (work_grok_acp._same_json): canonical
/// text with sorted keys - serde_json's Map is already key-ordered,
/// and its number rendering keeps 1 and 1.0 (and true and 1) apart,
/// exactly as Python's json.dumps comparison does.
fn same_json(left: &Value, right: &Value) -> bool {
    match (serde_json::to_string(left), serde_json::to_string(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// PurePosixPath parts: empty and '.' components drop out.
fn posix_parts(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

/// PurePosixPath str() form for a path whose parts contain no '..'.
fn posix_normalize(path: &str) -> String {
    let parts = posix_parts(path);
    if path.starts_with('/') {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

/// work_grok_acp._descriptor_path: a vendor descriptor path is only
/// ever a workspace path, the workspace root, or one of this
/// invocation's exact note paths.
fn descriptor_path(raw: Option<&Value>, allowed_notes: &[String]) -> Option<String> {
    let raw = raw.and_then(|v| v.as_str())?;
    if raw.is_empty() || raw.chars().count() > 4096 || raw.contains('\0') {
        return None;
    }
    if posix_parts(raw).iter().any(|part| *part == "..") {
        return None;
    }
    let absolute = if raw.starts_with('/') {
        posix_normalize(raw)
    } else {
        posix_normalize(&format!("{WORKSPACE}/{raw}"))
    };
    if absolute == WORKSPACE
        || absolute.starts_with(&format!("{WORKSPACE}/"))
        || allowed_notes.iter().any(|note| note == &absolute)
    {
        Some(absolute)
    } else {
        None
    }
}

const NATIVE_TOOL_KEYS: [&str; 7] = [
    "namespace",
    "name",
    "version",
    "kind",
    "label",
    "read_only",
    "input",
];

/// work_grok_acp._native_metadata: the frozen core of a native tool
/// descriptor ({namespace, name, version}), or None when the block
/// is not a well-formed v1 descriptor.
fn native_metadata(block: Option<&Value>) -> Option<Map<String, Value>> {
    let block = block.and_then(|v| v.as_object())?;
    if !["namespace", "name", "version"]
        .iter()
        .all(|key| block.contains_key(*key))
        || !block.keys().all(|key| NATIVE_TOOL_KEYS.contains(&key.as_str()))
    {
        return None;
    }
    let version = block.get("version")?;
    if version.as_i64() != Some(1) || version.is_f64() {
        return None;
    }
    for key in ["namespace", "name", "kind", "label"] {
        if let Some(value) = block.get(key) {
            let text = value.as_str()?;
            if text.is_empty()
                || text.chars().count() > 128
                || text.chars().any(|c| (c as u32) < 32)
            {
                return None;
            }
        }
    }
    if let Some(read_only) = block.get("read_only") {
        if !read_only.is_boolean() {
            return None;
        }
    }
    let mut core = Map::new();
    for key in ["namespace", "name", "version"] {
        core.insert(key.to_string(), block[key].clone());
    }
    Some(core)
}

/// work_grok_acp._descriptor_matches: a complete vendor descriptor
/// proves the captured arguments only when it reconstructs them
/// exactly (paths normalized through descriptor_path). The
/// descriptor is never substituted into the authoritative rawInput.
fn descriptor_matches(block: &Map<String, Value>, raw: &Map<String, Value>, allowed_notes: &[String]) -> bool {
    let Some(input) = block.get("input") else {
        return true;
    };
    let Some(value) = input.as_object() else {
        return false;
    };
    let raw_value = Value::Object(raw.clone());
    match block.get("name").and_then(|v| v.as_str()) {
        Some("read_file") => {
            let Some(normalized) = descriptor_path(raw.get("target_file"), allowed_notes) else {
                return false;
            };
            let mut expected = Map::new();
            expected.insert("path".to_string(), Value::String(normalized));
            for key in ["offset", "limit"] {
                if let Some(part) = raw.get(key) {
                    expected.insert(key.to_string(), part.clone());
                }
            }
            same_json(&Value::Object(value.clone()), &Value::Object(expected))
        }
        Some("list_dir") => {
            let Some(normalized) = descriptor_path(raw.get("target_directory"), allowed_notes) else {
                return false;
            };
            value.len() == 1
                && value.get("directory").and_then(|v| v.as_str()) == Some(normalized.as_str())
        }
        Some("grep") => {
            if !raw.get("pattern").map(|v| v.is_string()).unwrap_or(false) {
                return false;
            }
            let mut expected = Map::new();
            expected.insert("pattern".to_string(), raw["pattern"].clone());
            if let Some(path) = raw.get("path") {
                if !path.is_null() {
                    let Some(normalized) = descriptor_path(Some(path), allowed_notes) else {
                        return false;
                    };
                    expected.insert("path".to_string(), Value::String(normalized));
                }
            }
            same_json(&Value::Object(value.clone()), &Value::Object(expected))
        }
        Some("run_terminal_command") => {
            let keys: std::collections::HashSet<&str> =
                value.keys().map(|k| k.as_str()).collect();
            keys == raw.keys().map(|k| k.as_str()).collect()
                && keys.iter().all(|key| matches!(*key, "command" | "description"))
                && value
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|command| !command.is_empty() && command.chars().count() <= 16384)
                    .unwrap_or(false)
                && value.values().all(|v| v.is_string())
                && same_json(&Value::Object(value.clone()), &raw_value)
        }
        Some("write") | Some("search_replace") => {
            same_json(&Value::Object(value.clone()), &raw_value)
        }
        _ => false,
    }
}

/// work_grok_acp.same_tool_identity: progress annotations never
/// replace captured arguments or tool identity. Only the frozen
/// native core (namespace, name, version) may be restated.
pub fn same_tool_identity(previous: &Value, current: &Value) -> bool {
    if current.get("toolCallId") != previous.get("toolCallId") {
        return false;
    }
    let empty = Map::new();
    let (Some(original_meta), Some(meta)) = (
        match previous.get("_meta") {
            None => Some(&empty),
            Some(value) => value.as_object(),
        },
        match current.get("_meta") {
            None => Some(&empty),
            Some(value) => value.as_object(),
        },
    ) else {
        return false;
    };
    let core = native_metadata(original_meta.get("x.ai/tool"));
    let Some(value) = meta.get("x.ai/tool") else {
        return true;
    };
    let Some(value) = value.as_object() else {
        return false;
    };
    let Some(core) = core else {
        return false;
    };
    core.iter().all(|(key, expected)| {
        value.get(key).map(|v| same_json(v, expected)).unwrap_or(true)
    })
}

/// work_grok_acp.same_tool_input: affirmative permission proof,
/// independent of discarded progress data. Full current args must
/// match the captured args, or use one of the observed tagged
/// equivalents. A complete known vendor descriptor can also prove
/// the original args. Missing/unknown/contradictory proof grants
/// no permission.
pub fn same_tool_input(previous: &Value, current: &Value, allowed_notes: &[String]) -> bool {
    if !same_tool_identity(previous, current) {
        return false;
    }
    let empty = Map::new();
    let meta = current.get("_meta").and_then(|v| v.as_object()).unwrap_or(&empty);
    if !meta
        .keys()
        .all(|key| matches!(key.as_str(), "x.ai/tool" | "toolNotRun"))
    {
        return false;
    }
    let original = previous
        .get("_meta")
        .and_then(|v| v.as_object())
        .and_then(|meta| meta.get("x.ai/tool"))
        .and_then(|v| v.as_object());
    let raw = previous.get("rawInput").and_then(|v| v.as_object());
    let (Some(original), Some(raw)) = (original, raw) else {
        return false;
    };
    let augmented = meta.get("x.ai/tool").filter(|v| !v.is_null());
    if let Some(augmented) = augmented {
        let Some(augmented) = augmented.as_object() else {
            return false;
        };
        if !augmented
            .keys()
            .all(|key| NATIVE_TOOL_KEYS.contains(&key.as_str()))
        {
            return false;
        }
    }
    let augmented = augmented.and_then(|v| v.as_object());
    let with_input = |augmented: &Map<String, Value>| {
        let mut block = original.clone();
        block.insert("input".to_string(), augmented["input"].clone());
        descriptor_matches(&block, raw, allowed_notes)
    };
    if current.get("rawInput").is_none() {
        let Some(augmented) = augmented else {
            return false;
        };
        if !augmented.contains_key("input") {
            return false;
        }
        let complete: &[&str] = match original.get("name").and_then(|v| v.as_str()) {
            Some("read_file") => &["target_file", "offset", "limit"],
            Some("list_dir") => &["target_directory"],
            Some("grep") => &["pattern", "path"],
            Some("run_terminal_command") => &["command", "description"],
            Some("write") => &["file_path", "target_file", "path", "contents", "content"],
            Some("search_replace") => &[
                "file_path",
                "target_file",
                "path",
                "old_string",
                "new_string",
                "replace_all",
            ],
            _ => return false,
        };
        return raw.keys().all(|key| complete.contains(&key.as_str()))
            && with_input(augmented);
    }
    let tagged = &current["rawInput"];
    if same_json(tagged, &Value::Object(raw.clone())) {
        return match augmented {
            Some(augmented) if augmented.contains_key("input") => with_input(augmented),
            _ => true,
        };
    }
    let Some(tagged) = tagged.as_object() else {
        return false;
    };
    let (variant, defaults): (&str, Vec<(&str, Value)>) =
        match original.get("name").and_then(|v| v.as_str()) {
            Some("read_file") => ("ReadFile", Vec::new()),
            Some("list_dir") => ("ListDir", Vec::new()),
            Some("run_terminal_command") => (
                "Bash",
                vec![("is_background", Value::Bool(false))],
            ),
            Some("grep") => (
                "Grep",
                vec![
                    ("-i", Value::Bool(false)),
                    ("type", Value::Null),
                    ("multiline", Value::Bool(false)),
                    ("path", Value::Null),
                    ("glob", Value::Null),
                ],
            ),
            _ => return false,
        };
    let mut additions: Vec<(&str, Value)> = vec![("variant", Value::String(variant.to_string()))];
    additions.extend(defaults);
    let additions: Vec<(&str, Value)> = additions
        .into_iter()
        .filter(|(key, _)| !raw.contains_key(*key))
        .collect();
    let expected_keys: std::collections::HashSet<&str> = raw
        .keys()
        .map(|k| k.as_str())
        .chain(additions.iter().map(|(key, _)| *key))
        .collect();
    if tagged.keys().map(|k| k.as_str()).collect::<std::collections::HashSet<&str>>()
        != expected_keys
    {
        return false;
    }
    if !additions
        .iter()
        .all(|(key, value)| tagged.get(*key).map(|v| same_json(v, value)).unwrap_or(false))
    {
        return false;
    }
    let normalized: Map<String, Value> = tagged
        .iter()
        .filter(|(key, _)| !additions.iter().any(|(added, _)| *added == key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    same_json(&Value::Object(normalized), &Value::Object(raw.clone()))
        && match augmented {
            Some(augmented) if augmented.contains_key("input") => with_input(augmented),
            _ => true,
        }
}

/// Explicit parent-side read-only path proof; never expose the auth
/// root (work_grok_acp.NativePaths). Symlinks are rejected in every
/// component (including broken links). The OS sandbox remains the
/// authority against races and native tools that violate requests.
#[derive(Debug, Clone, PartialEq)]
pub struct NativePaths {
    pub workspace: PathBuf,
    pub notes: Vec<(String, PathBuf)>,
    pub git_configs: Vec<PathBuf>,
}

impl NativePaths {
    pub fn new(
        workspace: PathBuf,
        notes: Vec<(String, PathBuf)>,
        git_configs: Vec<PathBuf>,
    ) -> NativePaths {
        NativePaths {
            workspace,
            notes,
            git_configs,
        }
    }

    pub fn directory_chain(path: &Path) -> bool {
        if !path.is_absolute() {
            return false;
        }
        path.ancestors().all(|parent| {
            std::fs::symlink_metadata(parent)
                .map(|info| info.file_type().is_dir())
                .unwrap_or(false)
        })
    }

    pub fn git_allows(&self) -> bool {
        // Reads of built-ins can execute configured fsmonitor/diff
        // helpers. No Git subprocess is launched to prove a command
        // safe.
        for path in &self.git_configs {
            let Some(parent) = path.parent() else {
                return false;
            };
            if !Self::directory_chain(parent) {
                return false;
            }
            let info = match std::fs::symlink_metadata(path) {
                Ok(info) => info,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return false,
            };
            if !info.file_type().is_file() || info.len() > 65536 {
                return false;
            }
            let content = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return false,
            };
            if !harmless_git_config(&content) {
                return false;
            }
        }
        true
    }

    pub fn allows(&self, value: &Value, write: bool) -> bool {
        let Some(value) = value.as_str() else {
            return false;
        };
        if value.is_empty() || value.chars().count() > 4096 || value.contains('\x00') {
            return false;
        }
        let components: Vec<&str> = value
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if components.contains(&"..") {
            return false;
        }
        let base: Vec<&str> = WORKSPACE.split('/').filter(|p| !p.is_empty()).collect();
        let absolute: Vec<&str> = if value.starts_with('/') {
            components.clone()
        } else {
            base.iter().chain(components.iter()).cloned().collect()
        };
        let (root, parts): (PathBuf, Vec<String>) =
            if absolute == base || (absolute.len() > base.len() && absolute[..base.len()] == base[..])
            {
                let relative = &absolute[base.len()..];
                if relative
                    .iter()
                    .any(|p| matches!(*p, ".git" | ".grok" | ".claude" | ".cursor" | ".envrc"))
                {
                    return false;
                }
                (
                    self.workspace.clone(),
                    relative.iter().map(|p| p.to_string()).collect(),
                )
            } else {
                if write {
                    return false;
                }
                let normalized = format!("/{}", absolute.join("/"));
                let Some(host) = self
                    .notes
                    .iter()
                    .find(|(child, _)| *child == normalized)
                    .map(|(_, host)| host)
                else {
                    return false;
                };
                let (Some(parent), Some(name)) = (host.parent(), host.file_name()) else {
                    return false;
                };
                (parent.to_path_buf(), vec![name.to_string_lossy().into_owned()])
            };
        if !Self::directory_chain(&root) {
            return false;
        }
        let mut current = root;
        for (index, part) in parts.iter().enumerate() {
            current = current.join(part);
            let info = match std::fs::symlink_metadata(&current) {
                Ok(info) => info,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return write,
                Err(_) => return false,
            };
            let kind = info.file_type();
            if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
                return false;
            }
            if index + 1 < parts.len() && !kind.is_dir() {
                return false;
            }
        }
        true
    }
}

fn valid_identity(value: &str) -> bool {
    charset(value, 1, 128, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')
    })
}

/// `---[A-Z_]{1,24}---`
fn echo_marker(value: &str) -> bool {
    value
        .strip_prefix("---")
        .and_then(|rest| rest.strip_suffix("---"))
        .map(|middle| charset(middle, 1, 24, |c| c.is_ascii_uppercase() || c == '_'))
        .unwrap_or(false)
}

/// `-[1-9][0-9]?`
fn log_count(value: &str) -> bool {
    let bytes = value.as_bytes();
    (bytes.len() == 2 || bytes.len() == 3)
        && bytes[0] == b'-'
        && (b'1'..=b'9').contains(&bytes[1])
        && (bytes.len() == 2 || bytes[2].is_ascii_digit())
}

/// `test_[a-z0-9_]+\.py`
fn test_file(value: &str) -> bool {
    value
        .strip_prefix("test_")
        .and_then(|rest| rest.strip_suffix(".py"))
        .map(|middle| {
            !middle.is_empty()
                && middle
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .unwrap_or(false)
}

/// `test_[a-z0-9_]+(?:\.[A-Za-z0-9_]+){0,2}`
fn test_target(value: &str) -> bool {
    let mut groups = value.split('.');
    let Some(first) = groups.next() else {
        return false;
    };
    let head = first.strip_prefix("test_").unwrap_or("");
    if head.is_empty()
        || !head
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    let rest: Vec<&str> = groups.collect();
    rest.len() <= 2
        && rest.iter().all(|group| {
            !group.is_empty()
                && group
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// `[A-Za-z][A-Za-z0-9_]{0,127}`
fn cargo_argument(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        && value.chars().count() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn tail_eq(tail: &[String], want: &[&str]) -> bool {
    tail.len() == want.len() && tail.iter().zip(want.iter()).all(|(a, b)| a == b)
}

/// POSIX shlex splitting for the already metacharacter-free command
/// spelling: after the terminal() rejections no backslash, dollar or
/// backtick can remain, so quotes group literally.
fn shlex_split(segment: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = segment.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_token {
                    args.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        None => return None,
                        Some('\'') => break,
                        Some(other) => current.push(other),
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        None => return None,
                        Some('"') => break,
                        Some(other) => current.push(other),
                    }
                }
            }
            other => {
                in_token = true;
                current.push(other);
            }
        }
    }
    if in_token {
        args.push(current);
    }
    Some(args)
}

/// The deny-by-default native permission broker
/// (work_grok_acp.Permissions).
#[derive(Debug)]
pub struct Permissions {
    pub mode: String,
    pub paths: NativePaths,
    pub progress: bool,
}

impl Permissions {
    pub fn new(mode: &str, paths: NativePaths, progress: bool) -> Result<Permissions, String> {
        if !matches!(mode, "plan" | "review" | "implement") {
            return Err("Invalid Grok capability composition".to_string());
        }
        Ok(Permissions {
            mode: mode.to_string(),
            paths,
            progress,
        })
    }

    pub fn terminal(&self, command: Option<&Value>, cwd: Option<&Value>) -> bool {
        let cwd = match cwd {
            None => WORKSPACE,
            Some(value) => match value.as_str() {
                Some(value) => value,
                None => return false,
            },
        };
        if cwd != WORKSPACE {
            return false;
        }
        let Some(command) = command.and_then(|v| v.as_str()) else {
            return false;
        };
        if command.is_empty() || command.chars().count() > 16384 {
            return false;
        }
        // shlex is not a shell parser. Refuse all shell syntax outside
        // the one explicitly supported && sequence, even when quoted.
        // No interpolation.
        let command = command.replace("\\\"", "\"");
        // Reject expansion syntax even inside quotes rather than
        // pretending shlex proves what a shell will execute. Wildcards
        // can become explicit symlink operands which were absent from
        // the admitted argument list.
        if command.chars().any(|c| {
            matches!(
                c,
                '$' | '`'
                    | '\n'
                    | '\r'
                    | ';'
                    | '|'
                    | '<'
                    | '>'
                    | '\\'
                    | '\x00'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '~'
                    | '('
                    | ')'
            )
        }) {
            return false;
        }
        let bytes = command.as_bytes();
        if bytes.iter().enumerate().any(|(index, byte)| {
            *byte == b'&'
                && (index == 0 || bytes[index - 1] != b'&')
                && (index + 1 == bytes.len() || bytes[index + 1] != b'&')
        }) {
            return false;
        }
        let mut segments = Vec::new();
        for part in command.split("&&") {
            let Some(args) = shlex_split(part) else {
                return false;
            };
            segments.push(args);
        }
        !segments.is_empty()
            && segments.len() <= 16
            && segments.iter().all(|args| self.command(args))
    }

    fn command(&self, args: &[String]) -> bool {
        if args.is_empty() {
            return false;
        }
        if args[0] == "echo" {
            return args.len() == 2 && echo_marker(&args[1]);
        }
        if args.len() >= 2 && args[0] == "python3" && args[1] == format!("{STATE}/report_progress.py") {
            return self.progress
                && args.len() == 4
                && matches!(args[2].as_str(), "plan" | "progress")
                && !args[3].is_empty()
                && args[3].len() <= 4096;
        }
        if args[0] == "git" {
            if !self.paths.git_allows() {
                return false;
            }
            let tail = &args[1..];
            let fixed: &[&[&str]] = &[
                &["status"],
                &["status", "--short"],
                &["status", "--short", "--branch"],
                &["branch", "--show-current"],
                &["rev-parse", "HEAD"],
                &["worktree", "list"],
                &["ls-files"],
            ];
            if fixed.iter().any(|want| tail_eq(tail, want)) {
                return true;
            }
            if !tail.is_empty() && tail[0] == "log" {
                return tail.len() <= 5
                    && tail[1..]
                        .iter()
                        .all(|a| matches!(a.as_str(), "--oneline" | "--no-decorate" | "HEAD") || log_count(a));
            }
            if tail_eq(&tail[..tail.len().min(3)], &["grep", "-n", "--"])
                || tail_eq(&tail[..tail.len().min(4)], &["grep", "-n", "-F", "--"])
            {
                let prefix = if tail_eq(&tail[..tail.len().min(4)], &["grep", "-n", "-F", "--"]) {
                    4
                } else {
                    3
                };
                return tail.len() == prefix + 1
                    && !tail[prefix].is_empty()
                    && tail[prefix].chars().count() <= 2048;
            }
            if tail_eq(&tail[..tail.len().min(2)], &["show", "--stat"]) {
                return tail_eq(&tail[2..], &["HEAD"]);
            }
            if !tail.is_empty() && tail[0] == "diff" {
                let rest = &tail[1..];
                return rest.is_empty()
                    || tail_eq(rest, &["--stat"])
                    || tail_eq(rest, &["--name-only"])
                    || tail_eq(rest, &["--check"]);
            }
            return false;
        }
        if args[0] == "rg" {
            if tail_eq(&args[1..], &["--files"]) {
                return true;
            }
            let prefix = if args.len() >= 4 && tail_eq(&args[1..4], &["-n", "-F", "--"]) {
                4
            } else if args.len() >= 3 && tail_eq(&args[1..3], &["-n", "--"]) {
                3
            } else {
                0
            };
            return prefix > 0
                && (args.len() == prefix + 1 || args.len() == prefix + 2)
                && !args[prefix].is_empty()
                && args[prefix].chars().count() <= 2048
                && (args.len() == prefix + 1 || self.paths.allows(&json!(args[args.len() - 1].clone()), false));
        }
        if self.mode != "implement" {
            return false;
        }
        if args[0] == "make" {
            return tail_eq(&args[1..], &["ci-preflight"])
                || tail_eq(&args[1..], &["ci"])
                || tail_eq(&args[1..], &["ui-qa"]);
        }
        if args.len() >= 4 && tail_eq(&args[..4], &["python3", "-B", "-m", "unittest"]) {
            if args.len() >= 8
                && tail_eq(&args[4..8], &["discover", "-s", "deploy/mission-control/agent-runner", "-p"])
            {
                return (args.len() == 9 || args.len() == 10)
                    && test_file(&args[8])
                    && (args.len() == 9 || matches!(args[9].as_str(), "-q" | "-v"));
            }
            return args.len() > 4
                && args.len() <= 20
                && args[4..]
                    .iter()
                    .all(|a| matches!(a.as_str(), "-q" | "-v") || test_target(a));
        }
        if args.len() >= 2 && tail_eq(&args[..2], &["python3", "-B"]) {
            return tail_eq(&args[2..], &["deploy/mission-control/backend-suites.py", "--scope", "branch"]);
        }
        // Explicit compiler/test commands only, no arbitrary scripts,
        // shell, package installation, network/publishing or
        // user-provided executables.
        if args[0] == "cargo" && args.len() >= 2 {
            return matches!(args[1].as_str(), "check" | "build" | "test" | "clippy" | "fmt")
                && args.len() <= 16
                && args[2..].iter().all(|a| {
                    matches!(
                        a.as_str(),
                        "--workspace"
                            | "--all-targets"
                            | "--all-features"
                            | "--locked"
                            | "--offline"
                            | "--release"
                            | "--check"
                            | "--all"
                            | "--"
                            | "--nocapture"
                            | "--exact"
                    ) || cargo_argument(a)
                });
        }
        false
    }

    pub fn admit(&self, tool: &Value) -> bool {
        let Some(tool) = tool.as_object() else {
            return false;
        };
        let native = tool
            .get("_meta")
            .and_then(|meta| meta.as_object())
            .and_then(|meta| meta.get("x.ai/tool"))
            .and_then(|native| native.as_object());
        let name = native.and_then(|native| {
            if native.get("namespace").and_then(|v| v.as_str()) == Some("grok_build")
                && native.get("version").and_then(|v| v.as_i64()) == Some(1)
            {
                native.get("name").and_then(|v| v.as_str())
            } else {
                None
            }
        });
        let Some(name) = name else {
            return false;
        };
        let Some(raw) = tool.get("rawInput").and_then(|v| v.as_object()) else {
            return false;
        };
        if name == "run_terminal_command" {
            let fields: HashSet<&str> = ["command", "cwd", "timeout_ms", "max_output_chars", "description"]
                .into_iter()
                .collect();
            return raw.keys().all(|key| fields.contains(key.as_str()))
                && self.terminal(raw.get("command"), raw.get("cwd"));
        }
        if matches!(name, "read_file" | "list_dir" | "grep" | "write" | "search_replace") {
            let write = matches!(name, "write" | "search_replace");
            if write && self.mode != "implement" {
                return false;
            }
            let fields: &[&str] = match name {
                "read_file" => &["target_file", "offset", "limit"],
                "list_dir" => &["target_directory", "relative_workspace_path", "path"],
                "grep" => &[
                    "pattern",
                    "path",
                    "glob",
                    "head_limit",
                    "-A",
                    "-i",
                    "type",
                    "multiline",
                    "include_pattern",
                    "exclude_pattern",
                    "case_sensitive",
                ],
                "write" => &["file_path", "target_file", "path", "contents", "content"],
                _ => &["file_path", "target_file", "path", "old_string", "new_string", "replace_all"],
            };
            if !raw.keys().all(|key| fields.contains(&key.as_str())) {
                return false;
            }
            let pathfields: Vec<&str> = [
                "target_file",
                "target_directory",
                "relative_workspace_path",
                "file_path",
                "path",
            ]
            .into_iter()
            .filter(|key| raw.contains_key(*key))
            .collect();
            // A grep with no path (or an explicit null path) reads the
            // workspace itself; every other call names exactly one.
            let path: Value = if name == "grep"
                && (pathfields.is_empty()
                    || pathfields == ["path"] && raw["path"].is_null())
            {
                Value::String(WORKSPACE.to_string())
            } else if pathfields.len() == 1 {
                raw[pathfields[0]].clone()
            } else {
                return false;
            };
            if name == "grep" {
                let pattern = raw.get("pattern").and_then(|v| v.as_str());
                if !pattern
                    .map(|p| !p.is_empty() && p.chars().count() <= 8192)
                    .unwrap_or(false)
                {
                    return false;
                }
                for key in ["-i", "multiline", "case_sensitive"] {
                    if let Some(value) = raw.get(key) {
                        if !value.is_boolean() {
                            return false;
                        }
                    }
                }
                for (key, minimum, maximum) in [("head_limit", 1i64, 10000i64), ("-A", 0, 4096)] {
                    if let Some(value) = raw.get(key) {
                        if value.as_i64().map(|n| !(minimum..=maximum).contains(&n)).unwrap_or(true)
                            || value.is_f64()
                        {
                            return false;
                        }
                    }
                }
                if let Some(kind) = raw.get("type") {
                    if !kind.is_null() {
                        let Some(kind) = kind.as_str() else {
                            return false;
                        };
                        if kind.is_empty()
                            || kind.chars().count() > 64
                            || !kind
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                        {
                            return false;
                        }
                    }
                }
            }
            for key in ["include_pattern", "exclude_pattern", "glob"] {
                if name == "grep" && key == "glob" && raw.get(key).map(|v| v.is_null()).unwrap_or(false) {
                    continue;
                }
                let pattern = raw.get(key).cloned().unwrap_or(Value::String(String::new()));
                let Some(pattern) = pattern.as_str() else {
                    return false;
                };
                if pattern.chars().count() > 4096
                    || pattern.starts_with('/')
                    || pattern
                        .split('/')
                        .filter(|part| !part.is_empty() && *part != ".")
                        .any(|part| part == "..")
                {
                    return false;
                }
            }
            return self.paths.allows(&path, write);
        }
        false
    }
}

/// One fresh ACP session, serial host requests, bounded JSONL
/// frames (work_grok_acp.Transport). Only initialize/new/prompt and
/// permission replies can leave this adapter. No authenticate,
/// extension RPC, MCP, file or terminal service is offered.
#[derive(Debug)]
pub struct GrokAcpTransport {
    prompt: String,
    pub permissions: Permissions,
    buffer: Vec<u8>,
    total: usize,
    pub complete: bool,
    pub session: Option<String>,
    pub phase: &'static str,
    seen: HashSet<String>,
    calls: BTreeMap<String, Value>,
    pub usage: Option<Value>,
    pub cancellation: Option<String>,
    early: Vec<Value>,
    terminal_stop: Option<String>,
    pub quota_diagnostic: Option<Value>,
    outbound: Vec<u8>,
}

impl GrokAcpTransport {
    pub fn new(prompt: &str, permissions: Permissions) -> Result<GrokAcpTransport, String> {
        if prompt.len() > MAX_FRAME / 2 {
            return Err("Invalid ACP prompt bound".to_string());
        }
        let prompt = format!("{prompt}\n\nCONTROLLER PERMISSION BOUNDARY: {DIRECTION}");
        let outbound = wire(&json!({"jsonrpc": "2.0", "id": "host-initialize",
            "method": "initialize", "params": {
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false},
                                       "terminal": false},
                "clientInfo": {"name": "homelab-constrained-grok", "version": "1"}}}));
        Ok(GrokAcpTransport {
            prompt,
            permissions,
            buffer: Vec::new(),
            total: 0,
            complete: false,
            session: None,
            phase: "initialize",
            seen: HashSet::new(),
            calls: BTreeMap::new(),
            usage: None,
            cancellation: None,
            early: Vec::new(),
            terminal_stop: None,
            quota_diagnostic: None,
            outbound,
        })
    }

    pub fn drain(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outbound)
    }

    fn send(&mut self, value: &Value) -> Result<(), String> {
        let data = wire(value);
        if self.outbound.len() + data.len() > MAX_FRAME {
            return Err("ACP outbound bound exceeded".to_string());
        }
        self.outbound.extend_from_slice(&data);
        Ok(())
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<u8>, String> {
        self.total += chunk.len();
        if self.total > MAX_TOTAL {
            return Err("ACP output bound exceeded".to_string());
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=index).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() || line.len() > MAX_FRAME {
                return Err("Invalid ACP frame".to_string());
            }
            let value = parse_record(line, "grok").map_err(|error| error.to_string())?;
            events.extend(self.record(value)?);
        }
        if self.buffer.len() > MAX_FRAME {
            return Err("ACP frame bound exceeded".to_string());
        }
        let mut out = Vec::new();
        for event in &events {
            out.extend_from_slice(&wire(event));
        }
        Ok(out)
    }

    pub fn eof(&self) -> Result<(), String> {
        if !self.buffer.is_empty() || !self.complete {
            return Err("Incomplete ACP turn".to_string());
        }
        Ok(())
    }

    fn record(&mut self, value: Value) -> Result<Vec<Value>, String> {
        let object = value
            .as_object()
            .ok_or("Invalid ACP envelope")?;
        if object.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") || self.complete {
            return Err("Invalid ACP envelope".to_string());
        }
        if let Some(method) = object.get("method").filter(|v| !v.is_null()).cloned() {
            let Some(method) = method.as_str() else {
                return Err("Invalid ACP request".to_string());
            };
            if object.contains_key("result") || object.contains_key("error") {
                return Err("Invalid ACP request".to_string());
            }
            let params = object
                .get("params")
                .and_then(|v| v.as_object())
                .ok_or("Invalid ACP parameters")?;
            if self.session.is_none()
                && self.phase == "new"
                && !object.contains_key("id")
                && method == "session/update"
            {
                if self.early.len() >= 8 {
                    return Err("ACP early notification bound exceeded".to_string());
                }
                self.early.push(value.clone());
                return Ok(Vec::new());
            }
            if self.session.is_none()
                || params.get("sessionId").and_then(|v| v.as_str())
                    != self.session.as_deref()
            {
                return Err("ACP session changed".to_string());
            }
            if let Some(identity) = object.get("id").cloned() {
                return self.permission_request(&identity, method, params);
            }
            if method == "session/update" {
                return self.update(params.get("update"));
            }
            if method == "_x.ai/session/update" {
                return self.terminal_update(params);
            }
            return Err("Unsupported ACP notification".to_string());
        }
        // A host response.
        let allowed: HashSet<&str> = ["jsonrpc", "id", "result", "error"].into_iter().collect();
        if !object.keys().all(|key| allowed.contains(key.as_str()))
            || object.get("id").and_then(|v| v.as_str())
                != Some(&format!("host-{}", self.phase))
        {
            return Err("ACP host response identity changed".to_string());
        }
        if let Some(error) = object.get("error").cloned() {
            return self.error_response(&value, &error);
        }
        let result = object
            .get("result")
            .and_then(|v| v.as_object())
            .ok_or("Invalid ACP response")?;
        match self.phase {
            "initialize" => {
                let version = result.get("protocolVersion");
                if version.and_then(|v| v.as_i64()) != Some(1) || version.map(|v| v.is_number() && !v.is_i64() && !v.is_u64()).unwrap_or(false) {
                    return Err("Unsupported ACP version".to_string());
                }
                let meta = result.get("_meta").cloned().unwrap_or(Value::Object(Map::new()));
                let Some(meta) = meta.as_object() else {
                    return Err("ACP native capability identity changed".to_string());
                };
                if meta
                    .get("currentWorkingDirectory")
                    .and_then(|v| v.as_str())
                    .unwrap_or(WORKSPACE)
                    != WORKSPACE
                    || meta.get("agentVersion").and_then(|v| v.as_str()).unwrap_or("1.0.92")
                        != "1.0.92"
                {
                    return Err("ACP native capability identity changed".to_string());
                }
                self.phase = "new";
                self.send(&json!({"jsonrpc": "2.0", "id": "host-new",
                    "method": "session/new",
                    "params": {"cwd": WORKSPACE, "mcpServers": []}}))?;
                Ok(Vec::new())
            }
            "new" => {
                let session = result
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .filter(|text| {
                        uuid::Uuid::parse_str(text)
                            .map(|parsed| parsed.to_string() == *text)
                            .unwrap_or(false)
                    })
                    .ok_or("Invalid ACP session")?
                    .to_string();
                self.session = Some(session.clone());
                self.phase = "prompt";
                let prompt = std::mem::take(&mut self.prompt);
                self.send(&json!({"jsonrpc": "2.0", "id": "host-prompt",
                    "method": "session/prompt",
                    "params": {"sessionId": session,
                               "prompt": [{"type": "text", "text": prompt}]}}))?;
                let early = std::mem::take(&mut self.early);
                let mut events = Vec::new();
                for notification in early {
                    events.extend(self.record(notification)?);
                }
                Ok(events)
            }
            "prompt" => {
                let stop = result
                    .get("stopReason")
                    .and_then(|v| v.as_str())
                    .filter(|stop| STOP_REASONS.contains(stop))
                    .ok_or("Invalid ACP stop reason")?
                    .to_string();
                if let Some(terminal) = &self.terminal_stop {
                    if *terminal != stop {
                        return Err("ACP stop identity changed".to_string());
                    }
                }
                let meta = result.get("_meta").cloned().unwrap_or(Value::Object(Map::new()));
                let Some(meta) = meta.as_object() else {
                    return Err("Invalid ACP result metadata".to_string());
                };
                self.usage = terminal_usage(meta.get("usage"))?;
                self.complete = true;
                Ok(vec![json!({
                    "cancellationCategory": self.cancellation,
                    "type": "end",
                    "sessionId": self.session,
                    "cwd": WORKSPACE,
                    "requestId": "host-prompt",
                    "stopReason": stop,
                    "usage": self.usage,
                })])
            }
            _ => Err("Invalid ACP phase".to_string()),
        }
    }

    fn permission_request(
        &mut self,
        identity: &Value,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<Vec<Value>, String> {
        let key = match identity {
            Value::String(text) => {
                if !valid_identity(text) || text.starts_with("host-") {
                    return Err("Invalid ACP request identity".to_string());
                }
                format!("s:{text}")
            }
            Value::Number(number) => {
                if number.as_i64().is_none() && number.as_u64().is_none() {
                    return Err("Invalid ACP request identity".to_string());
                }
                format!("i:{number}")
            }
            _ => return Err("Invalid ACP request identity".to_string()),
        };
        if self.seen.contains(&key) || self.seen.len() >= 4096 {
            return Err("Invalid ACP request identity".to_string());
        }
        self.seen.insert(key);
        if method != "session/request_permission" {
            self.send(&json!({"jsonrpc": "2.0", "id": identity,
                "error": {"code": -32601, "message": "Client capability unavailable"}}))?;
            return Ok(Vec::new());
        }
        if self.phase != "prompt" {
            return Err("ACP permission outside turn".to_string());
        }
        let tool = params.get("toolCall").and_then(|v| v.as_object());
        let options = params.get("options").and_then(|v| v.as_array());
        let (Some(tool), Some(options)) = (tool, options) else {
            return Err("Invalid ACP permission".to_string());
        };
        if options.is_empty() || options.len() > 8 {
            return Err("Invalid ACP permission".to_string());
        }
        let call = tool
            .get("toolCallId")
            .and_then(|v| v.as_str())
            .ok_or("Unbound ACP tool request")?;
        let previous = self
            .calls
            .get(call)
            .cloned()
            .ok_or("Unbound ACP tool request")?;
        let tool_value = Value::Object(tool.clone());
        if !same_tool_identity(&previous, &tool_value) {
            return Err("ACP permission identity changed".to_string());
        }
        let allowed_notes: Vec<String> = self
            .permissions
            .paths
            .notes
            .iter()
            .map(|(child, _host)| child.clone())
            .collect();
        let admitted = same_tool_input(&previous, &tool_value, &allowed_notes)
            && self.permissions.admit(&previous);
        let wanted = if admitted { "allow_once" } else { "reject_once" };
        let ids: Vec<Option<&str>> = options
            .iter()
            .map(|option| {
                option
                    .as_object()
                    .and_then(|option| option.get("optionId"))
                    .and_then(|id| id.as_str())
            })
            .collect();
        let selected: Vec<&Map<String, Value>> = options
            .iter()
            .filter_map(|option| option.as_object())
            .filter(|option| option.get("kind").and_then(|v| v.as_str()) == Some(wanted))
            .collect();
        if ids.len() != options.len()
            || ids.iter().any(|id| id.map(|id| !valid_identity(id)).unwrap_or(true))
            || {
                let unique: HashSet<&str> = ids.iter().flatten().cloned().collect();
                unique.len() != ids.len()
            }
            || selected.len() != 1
        {
            return Err("Missing one-call ACP permission option".to_string());
        }
        let option_id = selected[0].get("optionId").cloned().unwrap_or(Value::Null);
        self.send(&json!({"jsonrpc": "2.0", "id": identity,
            "result": {"outcome": {"outcome": "selected", "optionId": option_id},
                       "_meta": {"homelab/permission": if admitted { "admitted" } else { DIRECTION }}}}))?;
        Ok(Vec::new())
    }

    fn terminal_update(&mut self, params: &Map<String, Value>) -> Result<Vec<Value>, String> {
        // Ignore no arbitrary extensions: recognized final metadata
        // alone may provide terminal counters; per-response spend is
        // never summed.
        let update = params.get("update").and_then(|v| v.as_object());
        let Some(update) = update else {
            return Err("Invalid ACP terminal metadata".to_string());
        };
        let stop = update.get("stop_reason").and_then(|v| v.as_str());
        if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("turn_completed")
            || stop.map(|stop| !STOP_REASONS.contains(&stop)).unwrap_or(true)
        {
            return Err("Invalid ACP terminal metadata".to_string());
        }
        let stop = stop.unwrap_or("").to_string();
        if self.terminal_stop.is_some() {
            return Err("Duplicate ACP terminal metadata".to_string());
        }
        self.terminal_stop = Some(stop.clone());
        if stop == "cancelled" {
            if let Some(meta) = params.get("_meta").and_then(|v| v.as_object()) {
                if meta.get("cancellationCategory").and_then(|v| v.as_str())
                    == Some("PermissionCancelled")
                {
                    self.cancellation = Some("permission_cancelled".to_string());
                }
            }
        }
        // Notification usage can be partial. Only the matching final
        // prompt result's cumulative _meta.usage grants spend
        // authority.
        Ok(Vec::new())
    }

    fn error_response(
        &mut self,
        value: &Value,
        error: &Value,
    ) -> Result<Vec<Value>, String> {
        let object = value.as_object().ok_or("Invalid ACP error response")?;
        let error = error.as_object().ok_or("Invalid ACP error response")?;
        let code = error.get("code").and_then(|v| v.as_i64());
        let message = error.get("message").and_then(|v| v.as_str());
        if object.contains_key("result")
            || code.is_none()
            || message.map(|text| text.len() > 4096).unwrap_or(true)
        {
            return Err("Invalid ACP error response".to_string());
        }
        let code = code.unwrap_or(0);
        let message = message.unwrap_or("");
        let lowered = message.to_lowercase();
        self.quota_diagnostic = quota_diagnostic(message);
        let canonical = if self.quota_diagnostic.is_some() {
            "usage_limit_reached"
        } else if ["unauthorized", "authentication_required", "authentication required", "invalid_grant", "not logged in"]
            .iter()
            .any(|key| lowered.contains(key))
        {
            "authentication_required"
        } else if ["permission_denied", "permission denied", "approval required"]
            .iter()
            .any(|key| lowered.contains(key))
        {
            "permission_denied"
        } else if code == -32800 {
            "cancelled"
        } else {
            "Grok ACP request failed."
        };
        self.complete = true;
        self.prompt.clear();
        let mut event = json!({"type": "error", "message": canonical});
        if let Some(session) = &self.session {
            event["sessionId"] = json!(session);
            event["cwd"] = json!(WORKSPACE);
            event["requestId"] = json!(format!("host-{}", self.phase));
        }
        Ok(vec![event])
    }

    fn update(&mut self, update: Option<&Value>) -> Result<Vec<Value>, String> {
        if self.phase != "prompt" {
            return Err("Invalid ACP update".to_string());
        }
        let Some(update) = update.and_then(|v| v.as_object()) else {
            return Err("Invalid ACP update".to_string());
        };
        let kind = update
            .get("sessionUpdate")
            .and_then(|v| v.as_str())
            .ok_or("Invalid ACP update kind")?;
        match kind {
            "agent_message_chunk" => {
                let content = update.get("content").and_then(|v| v.as_object());
                let text = content.and_then(|content| {
                    if content.get("type").and_then(|v| v.as_str()) == Some("text") {
                        content.get("text").and_then(|v| v.as_str())
                    } else {
                        None
                    }
                });
                let Some(text) = text else {
                    return Err("Invalid ACP text".to_string());
                };
                Ok(vec![json!({"type": "text", "data": text})])
            }
            "tool_call" => {
                let identity = update.get("toolCallId").and_then(|v| v.as_str());
                let Some(identity) = identity else {
                    return Err("Invalid ACP tool identity".to_string());
                };
                if !valid_identity(identity)
                    || self.calls.contains_key(identity)
                    || self.calls.len() >= 4096
                {
                    return Err("Invalid ACP tool identity".to_string());
                }
                // The captured call is the permission evidence; its
                // native metadata must be a well-formed descriptor.
                // (The stored Value is owned - the deepcopy in the
                // Python transport - so progress can never alias it.)
                let empty = Map::new();
                let meta = match update.get("_meta") {
                    None => &empty,
                    Some(value) => value
                        .as_object()
                        .ok_or("Invalid ACP native tool metadata")?,
                };
                if !meta
                    .keys()
                    .all(|key| matches!(key.as_str(), "x.ai/tool" | "toolNotRun"))
                    || (meta.contains_key("x.ai/tool")
                        && native_metadata(meta.get("x.ai/tool")).is_none())
                {
                    return Err("Invalid ACP native tool metadata".to_string());
                }
                self.calls
                    .insert(identity.to_string(), Value::Object(update.clone()));
                Ok(Vec::new())
            }
            "tool_call_update" => {
                let identity = update.get("toolCallId").and_then(|v| v.as_str());
                let Some(previous) = identity.and_then(|id| self.calls.get(id)) else {
                    return Err("Unbound ACP tool update".to_string());
                };
                if !same_tool_identity(previous, &Value::Object(update.clone())) {
                    return Err("ACP tool identity changed after binding".to_string());
                }
                // rawInput and vendor input in an update are bounded
                // display/progress annotations: neither merged nor
                // used later as permission evidence.
                Ok(Vec::new())
            }
            "user_message_chunk" | "agent_thought_chunk" | "plan" | "available_commands"
            | "current_mode_update" | "config_option_update" | "usage_update" => Ok(Vec::new()),
            _ => Err("Unsupported ACP update".to_string()),
        }
    }
}

/// The factory port the runner provider composes (work_grok_acp
/// .Factory.create). Implementations create one fresh transport per
/// attempt.
pub trait GrokTransportFactory {
    fn create(
        &self,
        mode: &str,
        workspace_path: &Path,
        git_configs: Vec<PathBuf>,
        notes: Vec<(String, PathBuf)>,
        prompt: &str,
        progress: bool,
    ) -> Result<GrokAcpTransport, String>;
}

/// The built-in factory (work_grok_acp.Factory).
pub struct GrokAcpFactory;

impl GrokTransportFactory for GrokAcpFactory {
    fn create(
        &self,
        mode: &str,
        workspace_path: &Path,
        git_configs: Vec<PathBuf>,
        notes: Vec<(String, PathBuf)>,
        prompt: &str,
        progress: bool,
    ) -> Result<GrokAcpTransport, String> {
        let paths = NativePaths::new(workspace_path.to_path_buf(), notes, git_configs);
        let permissions = Permissions::new(mode, paths, progress)?;
        GrokAcpTransport::new(prompt, permissions)
    }
}

/// The transport selection (work_grok_acp.selected_factory).
pub fn selected_factory(
    config: &crate::config::WorkerConfig,
) -> Result<Option<GrokAcpFactory>, String> {
    if config.grok_transport == "headless-v1" {
        return Ok(None);
    }
    if config.grok_transport != SELECTION {
        return Err("Unsupported Grok transport selection".to_string());
    }
    Ok(Some(GrokAcpFactory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const SID: &str = "00000000-0000-4000-8000-000000000041";

    fn tempdir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "forge-acp-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn tool(name: &str, raw: Value) -> Value {
        json!({"sessionUpdate": "tool_call", "toolCallId": "call-1", "rawInput": raw,
               "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build", "name": name}}})
    }

    fn initialize(transport: &mut GrokAcpTransport) -> (Value, Value, Value) {
        let first: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(first["method"], "initialize");
        transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-initialize",
                                "result": {"protocolVersion": 1}})))
            .unwrap();
        let new: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(new["params"], json!({"cwd": WORKSPACE, "mcpServers": []}));
        transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-new",
                                "result": {"sessionId": SID}})))
            .unwrap();
        let prompt: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(prompt["params"]["sessionId"], SID);
        (first, new, prompt)
    }

    fn update(transport: &mut GrokAcpTransport, value: Value) -> Result<Vec<u8>, String> {
        transport.feed(&wire(&json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": SID, "update": value}})))
    }

    fn permission(
        transport: &mut GrokAcpTransport,
        call: &Value,
        options: Option<Value>,
    ) -> Result<Vec<u8>, String> {
        let options = options.unwrap_or_else(|| {
            json!([
                {"optionId": "native-allow", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "native-reject", "name": "Reject once", "kind": "reject_once"},
                {"optionId": "native-always", "name": "Always", "kind": "allow_always"},
            ])
        });
        transport.feed(&wire(&json!({"jsonrpc": "2.0", "id": "permission-1",
            "method": "session/request_permission",
            "params": {"sessionId": SID, "toolCall": call, "options": options}})))
    }

    fn plan_permissions(root: &Path) -> (Permissions, PathBuf) {
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("file"), "source").unwrap();
        let note = root.join("approved-plan.md");
        std::fs::write(&note, "immutable plan").unwrap();
        let paths = NativePaths::new(
            workspace.clone(),
            vec![(format!("{STATE}/approved-plan.md"), note)],
            Vec::new(),
        );
        (Permissions::new("plan", paths, true).unwrap(), workspace)
    }

    #[test]
    fn compound_read_only_git_is_admitted() {
        let root = tempdir("compound");
        let (plan, _) = plan_permissions(&root);
        let command = "git status --short --branch && echo \\\"---BRANCH---\\\" && git branch --show-current && echo \\\"---HEAD---\\\" && git rev-parse HEAD && echo \\\"---LOG---\\\" && git log -5 --oneline && echo \\\"---WORKTREES---\\\" && git worktree list";
        assert!(plan.terminal(Some(&json!(command)), None));
        assert!(plan.admit(&tool(
            "run_terminal_command",
            json!({"command": command, "description": "read inventory"})
        )));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn shell_writes_substitutions_and_unknown_tools_rejected() {
        let root = tempdir("reject");
        let (plan, _) = plan_permissions(&root);
        for command in [
            "git status && git push",
            "git branch feature/evil",
            "git commit -am x",
            "git log --output=/tmp/leak",
            "git -c core.fsmonitor=evil status",
            "git status; touch x",
            "git status > /tmp/out",
            "git status | cat",
            "git status && $(id)",
            "git status && `id`",
            "git status &",
            "env git status",
            "sh -c \"git status\"",
            "cd /tmp && git status",
            "git status\nrm -rf x",
            "python3 -c print(1)",
            "curl https://example.com",
            "cargo test",
        ] {
            assert!(!plan.terminal(Some(&json!(command)), None), "{command}");
        }
        assert!(!plan.terminal(Some(&json!("git status")), Some(&json!("/tmp"))));
        for name in ["MCPTool", "mcp__read", "spawn_subagent", "web_search", "unknown"] {
            assert!(!plan.admit(&tool(name, json!({"path": "file"}))), "{name}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn shell_expansion_cannot_inject_rg_operands() {
        let root = tempdir("rg");
        let (plan, _) = plan_permissions(&root);
        for expression in ["*", "?", "[fl]*", "{file,linked-note}", "~", "@(file)"] {
            for quoted in [
                expression.to_string(),
                format!("'{expression}'"),
                format!("\"{expression}\""),
            ] {
                assert!(
                    !plan.terminal(Some(&json!(format!("rg -n -- {quoted}"))), None),
                    "{quoted}"
                );
            }
        }
        assert!(plan.terminal(Some(&json!("rg -n -- source")), None));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn native_paths_notes_and_symlinks_are_scoped() {
        let root = tempdir("paths");
        let (plan, workspace) = plan_permissions(&root);
        assert!(plan.admit(&tool("read_file", json!({"target_file": "file", "limit": 100}))));
        assert!(plan.admit(&tool(
            "read_file",
            json!({"target_file": format!("{STATE}/approved-plan.md")})
        )));
        for path in [
            format!("{STATE}/home/.grok/auth.json"),
            format!("{STATE}/prompt.txt"),
            "../outside".to_string(),
            "/etc/passwd".to_string(),
            ".git/config".to_string(),
        ] {
            assert!(
                !plan.admit(&tool("read_file", json!({"target_file": path}))),
                "{path}"
            );
        }
        symlink(root.join("approved-plan.md"), workspace.join("link")).unwrap();
        assert!(!plan.admit(&tool("read_file", json!({"target_file": "link"}))));
        assert!(!plan.admit(&tool(
            "read_file",
            json!({"target_file": "file", "path": "/etc/passwd"})
        )));
        let mut fake = tool("read_file", json!({"target_file": "file"}));
        fake["_meta"]["x.ai/tool"]["namespace"] = json!("mcp");
        assert!(!plan.admit(&fake));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn report_helper_and_implementation_capability() {
        let root = tempdir("helper");
        let (plan, _) = plan_permissions(&root);
        let command = format!("python3 {STATE}/report_progress.py plan 'Inspect current baseline'");
        assert!(plan.terminal(Some(&json!(command)), None));
        let no_progress = Permissions::new("plan", plan.paths.clone(), false).unwrap();
        assert!(!no_progress.terminal(Some(&json!(command)), None));
        assert!(!plan.terminal(Some(&json!(format!("{command} && git push"))), None));
        assert!(!plan.terminal(Some(&json!(format!("python3 {STATE}/other.py"))), None));
        assert!(!plan.admit(&tool("write", json!({"file_path": "file", "contents": "edit"}))));
        let implementation = Permissions::new("implement", plan.paths.clone(), false).unwrap();
        assert!(implementation.admit(&tool("write", json!({"file_path": "new", "contents": "edit"}))));
        assert!(implementation.admit(&tool(
            "search_replace",
            json!({"path": "file", "old_string": "source", "new_string": "edit"})
        )));
        assert!(!implementation.admit(&tool(
            "write",
            json!({"path": "file", "file_path": "/etc/outside", "contents": "edit"})
        )));
        assert!(implementation.terminal(Some(&json!("cargo test --offline --locked")), None));
        assert!(!implementation.terminal(Some(&json!("cargo test --config core.x=y")), None));
        assert!(!implementation.terminal(Some(&json!("cargo test --manifest-path /tmp/Cargo.toml")), None));
        assert!(!implementation.terminal(Some(&json!("cargo publish")), None));
        assert!(!implementation.admit(&tool(
            "write",
            json!({"file_path": format!("{STATE}/home/.grok/config.toml"), "contents": "x"})
        )));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_executable_config_is_not_approved() {
        let root = tempdir("gitconfig");
        let (plan, workspace) = plan_permissions(&root);
        let config = root.join("config");
        let scoped = Permissions::new(
            "plan",
            NativePaths::new(workspace, Vec::new(), vec![config.clone()]),
            false,
        )
        .unwrap();
        std::fs::write(&config, "[core]\n bare = true\n").unwrap();
        assert!(scoped.terminal(Some(&json!("git status")), None));
        for content in [
            "[core]\n fsmonitor = evil\n",
            "[include]\n path = /tmp/evil\n",
            "[core] fsmonitor = /tmp/fixture-never-executed\n",
            "[filter \"demo\"]\n clean = /tmp/fixture-never-executed\n",
            "[log]\n showSignature = true\n[gpg]\n program = /tmp/fixture-never-executed\n",
            "[core]\n attributesFile = /tmp/fixture-never-read\n",
            "[diff \"x\"]\n textconv = evil\n",
            "[alias]\n status = !evil\n",
        ] {
            std::fs::write(&config, content).unwrap();
            assert!(!scoped.terminal(Some(&json!("git status")), None), "{content}");
        }
        let _ = plan;
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn git_common_config_bound_for_bare_and_legacy_worktrees() {
        let root = tempdir("gitdirs");
        for common in [root.join("owned.git"), root.join("canonical/.git")] {
            let directory = common.join("worktrees/task");
            let (config, worktree, system) = git_config_paths(&directory).unwrap();
            assert_eq!(config, common.join("config"));
            assert_eq!(worktree, directory.join("config.worktree"));
            assert_eq!(system, PathBuf::from("/etc/gitconfig"));
        }
        assert!(git_config_paths(&root.join("arbitrary")).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn implementation_tools_do_not_enable_publish() {
        let root = tempdir("impl");
        let (plan, _) = plan_permissions(&root);
        let implement = Permissions::new("implement", plan.paths.clone(), false).unwrap();
        for command in [
            "make ci-preflight",
            "python3 -B -m unittest test_example -q",
            "cargo check --offline",
            "python3 -B -m unittest discover -s deploy/mission-control/agent-runner -p test_example.py -q",
            "python3 -B deploy/mission-control/backend-suites.py --scope branch",
        ] {
            assert!(implement.terminal(Some(&json!(command)), None), "{command}");
            assert!(!plan.terminal(Some(&json!(command)), None), "{command}");
        }
        for command in [
            "make deploy",
            "python3 -B script.py",
            "python3 -B -m unittest /tmp/test.py",
            "cargo run",
            "cargo test && git push",
            "make ci-preflight NETWORK=on",
        ] {
            assert!(!implement.terminal(Some(&json!(command)), None), "{command}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    fn make_transport() -> GrokAcpTransport {
        let paths = NativePaths::new(PathBuf::from("/nonexistent"), Vec::new(), Vec::new());
        let permissions = Permissions::new("plan", paths, false).unwrap();
        GrokAcpTransport::new("Bound task", permissions).unwrap()
    }

    #[test]
    fn initialize_session_prompt_shape() {
        let mut transport = make_transport();
        let (first, new, prompt) = initialize(&mut transport);
        assert_eq!(first["params"]["clientCapabilities"]["terminal"], false);
        assert_eq!(
            first["params"]["clientCapabilities"]["fs"],
            json!({"readTextFile": false, "writeTextFile": false})
        );
        assert!(new["params"].get("_meta").is_none());
        assert_eq!(new["params"]["mcpServers"], json!([]));
        let text = prompt["params"]["prompt"][0]["text"].as_str().unwrap();
        assert!(text.contains("CONTROLLER PERMISSION BOUNDARY"));
    }

    #[test]
    fn partial_messages_and_result_output_only() {
        let mut transport = make_transport();
        initialize(&mut transport);
        let raw = wire(&json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": SID, "update": {"sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "Public answer"}}}}));
        let mut output = Vec::new();
        for byte in &raw {
            output.extend(transport.feed(&[*byte]).unwrap());
        }
        let event: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(event, json!({"type": "text", "data": "Public answer"}));
        let silent = update(&mut transport, json!({"sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": "private prompt echo"}})).unwrap();
        assert!(silent.is_empty());
        let terminal = transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-prompt",
                                "result": {"stopReason": "end_turn"}})))
            .unwrap();
        let event: Value = serde_json::from_slice(&terminal).unwrap();
        assert_eq!(event["sessionId"], SID);
        assert!(event["usage"].is_null());
        transport.eof().unwrap();
        assert!(transport.feed(&raw).is_err());
    }

    #[test]
    fn denial_selects_reject_once_and_turn_continues() {
        let mut transport = make_transport();
        initialize(&mut transport);
        let call = tool("run_terminal_command", json!({"command": "git push"}));
        update(&mut transport, call.clone()).unwrap();
        permission(&mut transport, &call, None).unwrap();
        let reply: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(
            reply["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "native-reject"})
        );
        assert!(reply["result"]["_meta"]["homelab/permission"]
            .as_str()
            .unwrap()
            .contains("Do not use shell"));
        assert!(!transport.complete);
        let output = update(&mut transport, json!({"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "Useful alternative"}})).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("Useful alternative"));
    }

    #[test]
    fn allow_once_only_by_provided_option_identity() {
        let mut transport = make_transport();
        initialize(&mut transport);
        let call = tool("run_terminal_command", json!({"command": "git status"}));
        update(&mut transport, call.clone()).unwrap();
        permission(
            &mut transport,
            &call,
            Some(json!([
                {"optionId": "opaque-42", "name": "Once", "kind": "allow_once"},
                {"optionId": "bad-always", "name": "Always", "kind": "allow_always"},
            ])),
        )
        .unwrap();
        let reply: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(
            reply["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "opaque-42"})
        );
    }

    #[test]
    fn permission_identity_change_and_missing_option_fail() {
        // Changed rawInput at permission time.
        let mut transport = make_transport();
        initialize(&mut transport);
        let call = tool("run_terminal_command", json!({"command": "git status"}));
        update(&mut transport, call.clone()).unwrap();
        let changed = tool("run_terminal_command", json!({"command": "git push"}));
        // A changed command is not an identity break: the proof fails
        // and the answer is reject_once, not a protocol error.
        permission(&mut transport, &changed, None).unwrap();
        let reply: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(
            reply["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "native-reject"})
        );
        assert!(reply["result"]["_meta"]["homelab/permission"]
            .as_str()
            .unwrap()
            .contains("Do not use shell"));
        // No allow/reject-once option for the wanted kind.
        let mut transport = make_transport();
        initialize(&mut transport);
        update(&mut transport, call.clone()).unwrap();
        assert!(permission(
            &mut transport,
            &call,
            Some(json!([{"optionId": "x", "name": "Always", "kind": "allow_always"}]))
        )
        .is_err());
        // A changed rawInput in an update is discarded progress data:
        // it neither errors nor lends the original any new arguments.
        let mut transport = make_transport();
        initialize(&mut transport);
        update(&mut transport, call.clone()).unwrap();
        let silent = update(&mut transport, json!({"sessionUpdate": "tool_call_update",
            "toolCallId": "call-1", "rawInput": {"command": "git push"}})).unwrap();
        assert!(silent.is_empty());
        // Status metadata may progress without changing identity.
        let mut transport = make_transport();
        initialize(&mut transport);
        update(&mut transport, call.clone()).unwrap();
        let silent = update(&mut transport, json!({"sessionUpdate": "tool_call_update",
            "toolCallId": "call-1", "status": "failed",
            "_meta": {"toolNotRun": "cancelled"}})).unwrap();
        assert!(silent.is_empty());
        let changed = json!({"sessionUpdate": "tool_call_update", "toolCallId": "call-1",
            "_meta": {"x.ai/tool": {"name": "write"}}});
        assert!(update(&mut transport, changed).is_err());
    }

    #[test]
    fn descriptor_proves_terminal_arguments_and_mismatch_rejects() {
        let call = tool(
            "run_terminal_command",
            json!({"command": "git status", "description": "read inventory"}),
        );
        let descriptor = |command: &str| {
            json!({"toolCallId": "call-1",
                   "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build",
                       "name": "run_terminal_command",
                       "input": {"command": command, "description": "read inventory"}}}})
        };
        // A complete descriptor reconstructing the captured args
        // admits, even with no rawInput in the permission request.
        let mut transport = make_transport();
        initialize(&mut transport);
        update(&mut transport, call.clone()).unwrap();
        permission(&mut transport, &descriptor("git status"), None).unwrap();
        let reply: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(
            reply["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "native-allow"})
        );
        // The same descriptor naming a different command proves
        // nothing: reject_once.
        let mut transport = make_transport();
        initialize(&mut transport);
        update(&mut transport, call.clone()).unwrap();
        permission(&mut transport, &descriptor("git push"), None).unwrap();
        let reply: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(
            reply["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "native-reject"})
        );
    }

    #[test]
    fn descriptor_and_tagged_variants_prove_captured_input() {
        let previous = |name: &str, raw: Value| {
            json!({"toolCallId": "call-1", "rawInput": raw,
                   "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build",
                       "name": name}}})
        };
        let notes: Vec<String> = Vec::new();
        // Tagged Bash equivalent with its observed defaults.
        let terminal = previous("run_terminal_command", json!({"command": "git status"}));
        let tagged = json!({"toolCallId": "call-1",
            "rawInput": {"variant": "Bash", "command": "git status", "is_background": false}});
        assert!(same_tool_input(&terminal, &tagged, &notes));
        let background = json!({"toolCallId": "call-1",
            "rawInput": {"variant": "Bash", "command": "git status", "is_background": true}});
        assert!(!same_tool_input(&terminal, &background, &notes));
        let extra = json!({"toolCallId": "call-1",
            "rawInput": {"variant": "Bash", "command": "git status",
                         "is_background": false, "extra": 1}});
        assert!(!same_tool_input(&terminal, &extra, &notes));
        // JSON types are part of the proof: true is not 1.
        let read = previous("read_file", json!({"target_file": "a.rs", "limit": 1}));
        let aliased = json!({"toolCallId": "call-1",
            "rawInput": {"target_file": "a.rs", "limit": true}});
        assert!(!same_tool_input(&read, &aliased, &notes));
        // A read descriptor normalizes the path against the symbolic
        // workspace root; it cannot redirect outside it, omit a
        // captured argument, or borrow a note path it was not given.
        let read = previous(
            "read_file",
            json!({"target_file": "src/main.rs", "offset": 0, "limit": 10}),
        );
        let described = |input: Value| {
            json!({"toolCallId": "call-1",
                   "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build",
                       "name": "read_file", "input": input}}})
        };
        assert!(same_tool_input(
            &read,
            &described(json!({"path": "/run/forge-coding-workspace/src/main.rs",
                              "offset": 0, "limit": 10})),
            &notes
        ));
        assert!(!same_tool_input(
            &read,
            &described(json!({"path": "/etc/passwd", "offset": 0, "limit": 10})),
            &notes
        ));
        assert!(!same_tool_input(
            &read,
            &described(json!({"path": "/run/forge-coding-workspace/src/main.rs",
                              "offset": 0})),
            &notes
        ));
        let note = "/run/forge-coding-attempt/approved-plan.md";
        let note_read = previous("read_file", json!({"target_file": note}));
        let note_described = described(json!({"path": note}));
        assert!(!same_tool_input(&note_read, &note_described, &notes));
        assert!(same_tool_input(
            &note_read,
            &note_described,
            &[note.to_string()]
        ));
        // Unknown authority fields in the request metadata fail closed.
        let mut unknown = described(json!({"path": "/run/forge-coding-workspace/src/main.rs",
                                           "offset": 0, "limit": 10}));
        unknown["_meta"]["surprise"] = json!(true);
        assert!(!same_tool_input(&read, &unknown, &notes));
    }

    #[test]
    fn initial_native_metadata_is_validated() {
        let mut transport = make_transport();
        initialize(&mut transport);
        let partial = json!({"sessionUpdate": "tool_call", "toolCallId": "call-9",
            "rawInput": {"command": "git status"},
            "_meta": {"x.ai/tool": {"name": "run_terminal_command"}}});
        assert!(update(&mut transport, partial)
            .unwrap_err()
            .contains("Invalid ACP native tool metadata"));
        let mut transport = make_transport();
        initialize(&mut transport);
        let extra = json!({"sessionUpdate": "tool_call", "toolCallId": "call-9",
            "rawInput": {"command": "git status"},
            "_meta": {"x.ai/tool": {"version": 1, "namespace": "grok_build",
                      "name": "run_terminal_command"}, "surprise": 1}});
        assert!(update(&mut transport, extra)
            .unwrap_err()
            .contains("Invalid ACP native tool metadata"));
    }

    #[test]
    fn grep_admission_bounds() {
        let root = tempdir("grep");
        let (plan, _) = plan_permissions(&root);
        // No path: the grep reads the workspace itself; the observed
        // vendor defaults are admitted within their bounds.
        assert!(plan.admit(&tool(
            "grep",
            json!({"pattern": "needle", "glob": null, "-i": false,
                   "head_limit": 25, "type": "rust"})
        )));
        assert!(!plan.admit(&tool("grep", json!({"pattern": ""}))));
        assert!(!plan.admit(&tool(
            "grep",
            json!({"pattern": "needle", "head_limit": 0})
        )));
        assert!(!plan.admit(&tool(
            "grep",
            json!({"pattern": "needle", "-A": 5000})
        )));
        assert!(!plan.admit(&tool(
            "grep",
            json!({"pattern": "needle", "type": "not a type!"})
        )));
        assert!(!plan.admit(&tool(
            "grep",
            json!({"pattern": "needle", "glob": "../outside"})
        )));
        assert!(!plan.admit(&tool(
            "grep",
            json!({"pattern": "needle", "-i": "yes"})
        )));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn early_notification_replayed_after_session() {
        let mut transport = make_transport();
        transport.drain();
        transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-initialize",
                                "result": {"protocolVersion": 1}})))
            .unwrap();
        transport.drain();
        update(&mut transport, json!({"sessionUpdate": "available_commands",
            "availableCommands": []})).unwrap();
        transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-new",
                                "result": {"sessionId": SID}})))
            .unwrap();
        let prompt: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(prompt["method"], "session/prompt");
    }

    #[test]
    fn notification_spend_never_becomes_turn_spend() {
        let mut transport = make_transport();
        initialize(&mut transport);
        transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "method": "_x.ai/session/update",
                "params": {"sessionId": SID, "update": {"sessionUpdate": "turn_completed",
                    "stop_reason": "end_turn", "usage": {"inputTokens": 99999}}}})))
            .unwrap();
        let event = transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-prompt",
                                "result": {"stopReason": "end_turn"}})))
            .unwrap();
        let event: Value = serde_json::from_slice(&event).unwrap();
        assert!(event["usage"].is_null());
    }

    #[test]
    fn terminal_usage_projection_and_invalid_totals() {
        let usage = terminal_usage(Some(&json!({"inputTokens": 10061,
            "cachedReadTokens": 4224, "cacheCreationTokens": 0,
            "outputTokens": 808, "totalTokens": 10869})))
            .unwrap()
            .unwrap();
        assert_eq!(
            usage,
            json!({"input_tokens": 5837, "cache_read_input_tokens": 4224,
                   "cache_creation_input_tokens": 0, "output_tokens": 808,
                   "total_tokens": 10869})
        );
        for bad in [
            json!({"inputTokens": 1, "cachedReadTokens": 2, "cacheCreationTokens": 0}),
            json!({"inputTokens": 10, "outputTokens": 5, "totalTokens": 14}),
            json!({"inputTokens": true}),
        ] {
            assert!(terminal_usage(Some(&bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn client_effect_rpc_is_rejected_not_executed() {
        let mut transport = make_transport();
        initialize(&mut transport);
        transport
            .feed(&wire(&json!({"jsonrpc": "2.0", "id": "terminal-x",
                "method": "terminal/create",
                "params": {"sessionId": SID, "command": "id"}})))
            .unwrap();
        let reply: Value = serde_json::from_slice(&transport.drain()).unwrap();
        assert_eq!(reply["error"]["code"], -32601);
    }

    #[test]
    fn native_errors_classified_without_raw_prose() {
        for (message, code, canonical) in [
            ("Authentication required PRIVATE", -32000, "authentication_required"),
            ("usage_limit_reached PRIVATE resets in 1h", -32000, "usage_limit_reached"),
            ("PRIVATE", -32800, "cancelled"),
        ] {
            let mut transport = make_transport();
            initialize(&mut transport);
            let output = transport
                .feed(&wire(&json!({"jsonrpc": "2.0", "id": "host-prompt",
                    "error": {"code": code, "message": message}})))
                .unwrap();
            assert!(!String::from_utf8_lossy(&output).contains("PRIVATE"));
            let event: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(event["message"], canonical);
            if canonical == "usage_limit_reached" {
                assert_eq!(
                    transport.quota_diagnostic.as_ref().unwrap()["retry_after_seconds"],
                    3600
                );
            }
        }
    }

    #[test]
    fn malformed_frames_fail_closed() {
        for raw in [
            b"{\"jsonrpc\":\"2.0\",\"id\":\"host-initialize\",\"id\":\"x\",\"result\":{}}\n".to_vec(),
            wire(&json!({"jsonrpc": "2.0", "id": "wrong", "result": {}})),
            wire(&json!({"jsonrpc": "2.0", "id": "host-initialize",
                         "result": {"protocolVersion": true}})),
            b"{\"x\":NaN}\n".to_vec(),
            b"[]\n".to_vec(),
            vec![b'x'; MAX_FRAME + 1],
        ] {
            let mut transport = make_transport();
            transport.drain();
            assert!(transport.feed(&raw).is_err(), "{} bytes", raw.len());
        }
        let mut fresh = make_transport();
        initialize(&mut fresh);
        assert!(fresh.eof().is_err());
    }
}
