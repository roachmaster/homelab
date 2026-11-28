//! Studio catalog, project admission and dispatch (studio.py's
//! Catalog and Service). Errors are (status, code) pairs mirroring
//! the Python Rejected exception; database failures map to 503 at
//! the handler, as psycopg2.Error does.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::artifact;
use crate::store::{BuildRow, BuildStore, StoreError};

pub const PROFILE: &str = "static-web";

pub struct Rejected {
    pub status: u16,
    pub code: String,
}

impl Rejected {
    fn new(status: u16, code: &str) -> Rejected {
        Rejected {
            status,
            code: code.to_string(),
        }
    }
}

type ServiceResult<T> = Result<T, Rejected>;

fn invalid() -> Rejected {
    Rejected::new(400, "invalid_request")
}

fn not_found() -> Rejected {
    Rejected::new(404, "not_found")
}

impl From<StoreError> for Rejected {
    fn from(error: StoreError) -> Rejected {
        match error {
            StoreError::Rejected { status, code } => Rejected { status, code },
            // psycopg2.Error maps to 503 unavailable in the handler.
            StoreError::Db(_) => Rejected::new(503, "unavailable"),
        }
    }
}

pub fn alias_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

pub fn revision_ok(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn build_id_ok(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let lengths = [8, 4, 4, 4, 12];
    parts.iter().zip(lengths).all(|(part, length)| {
        part.len() == length
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub fn intent_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn checked_keys(body: &Value, required: &[&str], optional: &[&str]) -> ServiceResult<()> {
    let object = body.as_object().ok_or_else(invalid)?;
    for key in required {
        if !object.contains_key(*key) {
            return Err(invalid());
        }
    }
    for key in object.keys() {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

#[derive(Clone)]
pub struct Project {
    pub node: String,
    pub project: String,
    pub revision: String,
    pub repository: PathBuf,
    pub directory: String,
}

impl Project {
    pub fn view(&self) -> Value {
        json!({
            "node": self.node,
            "project": self.project,
            "revision": self.revision,
            "profiles": [PROFILE],
        })
    }

    pub fn source_recipe(&self) -> String {
        // sha256 of json.dumps([1, PROFILE, str(repository), directory])
        // with Python's default separators.
        let encoded = format!(
            "[1, {}, {}, {}]",
            serde_json::to_string(PROFILE).unwrap_or_default(),
            serde_json::to_string(&self.repository.to_string_lossy()).unwrap_or_default(),
            serde_json::to_string(&self.directory).unwrap_or_default(),
        );
        artifact::sha256_hex(encoded.as_bytes())
    }
}

#[derive(Clone)]
pub struct Catalog {
    pub projects: BTreeMap<(String, String), Project>,
    pub max_builds: i32,
}

impl Catalog {
    pub fn load(config: &Value) -> Result<Catalog, String> {
        let projects = config
            .get("projects")
            .and_then(Value::as_array)
            .filter(|list| (1..=32).contains(&list.len()))
            .ok_or_else(|| "invalid Studio catalog".to_string())?;
        let max_builds = config
            .get("max_builds_per_project")
            .cloned()
            .unwrap_or(json!(256));
        if !is_int(&max_builds) {
            return Err("invalid Studio catalog".to_string());
        }
        let max_builds = max_builds.as_i64().unwrap_or(0);
        if !(1..=4096).contains(&max_builds) {
            return Err("invalid Studio catalog".to_string());
        }
        let mut approved = BTreeMap::new();
        for value in projects {
            checked_keys(
                value,
                &["node", "project", "revision", "repository", "directory"],
                &[],
            )
            .map_err(|_| "invalid Studio project".to_string())?;
            let get = |name: &str| value.get(name).and_then(Value::as_str).unwrap_or("");
            let (node, project, revision) = (get("node"), get("project"), get("revision"));
            let (repository, directory) = (get("repository"), get("directory"));
            let repository_path = PathBuf::from(repository);
            if !alias_ok(node)
                || !alias_ok(project)
                || !revision_ok(revision)
                || !repository_path.is_absolute()
                || !artifact::path_ok(directory)
                || approved.contains_key(&(node.to_string(), project.to_string()))
            {
                return Err("invalid Studio project".to_string());
            }
            approved.insert(
                (node.to_string(), project.to_string()),
                Project {
                    node: node.to_string(),
                    project: project.to_string(),
                    revision: revision.to_string(),
                    repository: repository_path,
                    directory: directory.to_string(),
                },
            );
        }
        Ok(Catalog {
            projects: approved,
            max_builds: max_builds as i32,
        })
    }

    pub fn get(&self, value: Option<&Value>) -> ServiceResult<((String, String), Project)> {
        let value = value.ok_or_else(invalid)?;
        checked_keys(value, &["node", "project"], &[])?;
        let node = value.get("node").and_then(Value::as_str).unwrap_or("");
        let project = value.get("project").and_then(Value::as_str).unwrap_or("");
        if !alias_ok(node) || !alias_ok(project) {
            return Err(invalid());
        }
        let key = (node.to_string(), project.to_string());
        self.projects
            .get(&key)
            .cloned()
            .map(|project| (key, project))
            .ok_or_else(not_found)
    }
}

const ROUTES: [(&str, &str, &str); 6] = [
    (
        "game.project.list",
        "game_project_list",
        "game_projects_listed",
    ),
    (
        "game.project.get",
        "game_project_get",
        "game_project_opened",
    ),
    (
        "game.build.submit",
        "game_build_submit",
        "game_build_submitted",
    ),
    ("game.build.get", "game_build_get", "game_build_status"),
    (
        "game.build.cancel",
        "game_build_cancel",
        "game_build_cancelled",
    ),
    (
        "game.artifact.get",
        "game_artifact_get",
        "game_artifact_opened",
    ),
];

/// The Studio control API. Its store is a port, bound by the
/// composition root (PostgreSQL in production, fakes in tests).
pub struct Service {
    store: Arc<dyn BuildStore>,
    catalog: Catalog,
    preview_origin: String,
    signing_key: Vec<u8>,
}

impl Service {
    pub fn new(
        store: Arc<dyn BuildStore>,
        catalog: Catalog,
        preview_origin: String,
        signing_key: Vec<u8>,
    ) -> Service {
        Service {
            store,
            catalog,
            preview_origin,
            signing_key,
        }
    }

    fn artifact_view(&self, row: &BuildRow) -> Value {
        match &row.artifact_digest {
            None => Value::Null,
            Some(digest) => {
                let token = artifact::sign(&self.signing_key, digest, None);
                json!({
                    "sha256": digest,
                    "bytes": row.artifact_bytes,
                    "preview_url": format!(
                        "{}/preview/{}/{}/index.html",
                        self.preview_origin, digest, token
                    ),
                })
            }
        }
    }

    fn build_view(&self, row: &BuildRow) -> Value {
        json!({
            "build_id": row.build_id,
            "project": {"node": row.node_alias, "project": row.project_alias},
            "revision": row.revision,
            "profile": row.profile,
            "state": row.state,
            "events": row
                .events
                .iter()
                .map(|event| json!({"sequence": event.sequence, "state": event.state}))
                .collect::<Vec<_>>(),
            "artifact": self.artifact_view(row),
        })
    }

    pub fn dispatch(
        &self,
        destination: &str,
        request_id: &str,
        body: &Value,
    ) -> Result<(u16, Value), Rejected> {
        let Some((_, request_kind, reply_kind)) =
            ROUTES.iter().find(|(name, _, _)| *name == destination)
        else {
            return Err(not_found());
        };
        let object = body.as_object().ok_or_else(invalid)?;
        if object.get("kind").and_then(Value::as_str) != Some(request_kind)
            || object.get("request_id").and_then(Value::as_str) != Some(request_id)
        {
            return Err(invalid());
        }
        let mut fields = Map::new();
        let mut status = 200u16;
        if destination == "game.project.list" {
            checked_keys(body, &["kind", "request_id"], &[])?;
            fields.insert(
                "projects".to_string(),
                Value::Array(
                    self.catalog
                        .projects
                        .values()
                        .map(|project| project.view())
                        .collect(),
                ),
            );
        } else {
            let (key, project) = self.catalog.get(object.get("project"))?;
            let common = ["kind", "request_id", "project"];
            if destination == "game.project.get" {
                checked_keys(body, &common, &[])?;
                fields.insert("project".to_string(), project.view());
            } else if destination == "game.build.submit" {
                let mut required = common.to_vec();
                required.extend(["revision", "profile", "idempotency_key"]);
                checked_keys(body, &required, &[])?;
                let revision = object.get("revision").and_then(Value::as_str).unwrap_or("");
                let profile = object.get("profile").and_then(Value::as_str).unwrap_or("");
                let intent = object
                    .get("idempotency_key")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !revision_ok(revision) || profile != PROFILE || !intent_ok(intent) {
                    return Err(invalid());
                }
                let record = self.store.submit(
                    &key,
                    revision,
                    profile,
                    intent,
                    self.catalog.max_builds,
                    &project.revision,
                    &project.source_recipe(),
                )?;
                fields.insert("build".to_string(), self.build_view(&record));
                status = 202;
            } else {
                let optional: &[&str] = if destination == "game.build.get" {
                    &["after_sequence"]
                } else {
                    &[]
                };
                let mut required = common.to_vec();
                required.push("build_id");
                checked_keys(body, &required, optional)?;
                let build_id = object.get("build_id").and_then(Value::as_str).unwrap_or("");
                let cursor = object.get("after_sequence").cloned().unwrap_or(json!(0));
                if !build_id_ok(build_id)
                    || !is_int(&cursor)
                    || !(0..=i64::from(i32::MAX)).contains(&cursor.as_i64().unwrap_or(-1))
                {
                    return Err(invalid());
                }
                if destination == "game.build.cancel" {
                    let record = self.store.cancel(&key, build_id)?;
                    fields.insert("build".to_string(), self.build_view(&record));
                } else {
                    let record =
                        self.store
                            .status(&key, build_id, cursor.as_i64().unwrap_or(0) as i32)?;
                    if destination == "game.artifact.get" {
                        let preview = self.artifact_view(&record);
                        if preview.is_null() {
                            return Err(not_found());
                        }
                        fields.insert("artifact".to_string(), preview);
                    } else {
                        fields.insert("build".to_string(), self.build_view(&record));
                    }
                }
            }
        }
        let mut reply = Map::new();
        reply.insert("kind".to_string(), json!(reply_kind));
        reply.insert("request_id".to_string(), json!(request_id));
        for (key, value) in fields {
            reply.insert(key, value);
        }
        Ok((status, Value::Object(reply)))
    }
}

/// Parse JSON rejecting duplicate object keys (Python's
/// object_pairs_hook=unique).
pub fn parse_strict_json(raw: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(raw).map_err(|error| error.to_string())?;
    check_duplicates(text)?;
    serde_json::from_str(text).map_err(|error| error.to_string())
}

fn check_duplicates(text: &str) -> Result<(), String> {
    let bytes = text.as_bytes();
    let mut stack: Vec<Option<std::collections::HashSet<String>>> = Vec::new();
    let mut index = 0usize;
    let mut expect_key = false;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                stack.push(Some(std::collections::HashSet::new()));
                expect_key = true;
                index += 1;
            }
            b'[' => {
                stack.push(None);
                expect_key = false;
                index += 1;
            }
            b'}' | b']' => {
                stack.pop();
                expect_key = false;
                index += 1;
            }
            b':' => {
                expect_key = false;
                index += 1;
            }
            b',' => {
                expect_key = matches!(stack.last(), Some(Some(_)));
                index += 1;
            }
            b'"' => {
                let (value, next) = scan_string(text, index)?;
                if expect_key {
                    if let Some(Some(keys)) = stack.last_mut() {
                        if !keys.insert(value) {
                            return Err("duplicate JSON field".to_string());
                        }
                    }
                    expect_key = false;
                }
                index = next;
            }
            _ => {
                index += 1;
            }
        }
    }
    Ok(())
}

fn scan_string(text: &str, start: usize) -> Result<(String, usize), String> {
    let bytes = text.as_bytes();
    let mut index = start + 1;
    let mut out = String::new();
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok((out, index + 1)),
            b'\\' => {
                let escaped = bytes.get(index + 1).copied().unwrap_or(0);
                match escaped {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'u' => {
                        let hex = text.get(index + 2..index + 6).ok_or("bad unicode escape")?;
                        let code = u32::from_str_radix(hex, 16)
                            .map_err(|_| "bad unicode escape".to_string())?;
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        index += 4;
                    }
                    _ => return Err("bad escape".to_string()),
                }
                index += 2;
            }
            _ => {
                // Copy one UTF-8 character.
                let ch_len = utf8_len(bytes[index]);
                out.push_str(&text[index..index + ch_len]);
                index += ch_len;
            }
        }
    }
    Err("unterminated string".to_string())
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_json_rejects_duplicates() {
        assert!(parse_strict_json(br#"{"a": 1, "a": 2}"#).is_err());
        assert!(parse_strict_json(br#"{"a": {"b": 1}, "c": [1, 2]}"#).is_ok());
        assert!(parse_strict_json(br#"{"a": 1, "\u0061": 2}"#).is_err());
    }

    #[test]
    fn validators() {
        assert!(alias_ok("node-1.x"));
        assert!(!alias_ok("bad name"));
        assert!(revision_ok(&"a".repeat(40)));
        assert!(!revision_ok(&"A".repeat(40)));
        assert!(build_id_ok("12345678-1234-1234-1234-123456789012"));
        assert!(!build_id_ok("12345678-1234-1234-1234-12345678901"));
        assert!(intent_ok("key.1"));
        assert!(!intent_ok(""));
    }

    #[test]
    fn source_recipe_shape() {
        let project = Project {
            node: "n".to_string(),
            project: "p".to_string(),
            revision: "a".repeat(40),
            repository: PathBuf::from("/srv/repo"),
            directory: "game".to_string(),
        };
        // Python: sha256(json.dumps([1, "static-web", "/srv/repo", "game"]))
        let expected = artifact::sha256_hex(br#"[1, "static-web", "/srv/repo", "game"]"#);
        assert_eq!(project.source_recipe(), expected);
    }
}
