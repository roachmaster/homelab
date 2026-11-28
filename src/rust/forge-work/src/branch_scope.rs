//! Git branch naming and workflow branch scope (the module-level
//! helpers of work_external_workflow.py).

use crate::canonical::sha256_hex;
use crate::validation::is_commit;
use serde_json::json;

/// [A-Za-z0-9][A-Za-z0-9._/-]{0,127}
fn branch_name_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

/// [a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?
fn branch_slug_valid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 58 {
        return false;
    }
    let lower_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !lower_alnum(bytes[0]) {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }
    bytes[1..]
        .iter()
        .all(|b| lower_alnum(*b) || matches!(b, b'.' | b'_' | b'-'))
        && lower_alnum(bytes[bytes.len() - 1])
}

pub fn validate_branch_name(value: &str) -> Result<&str, String> {
    let invalid = || "Invalid Git branch name".to_string();
    if !branch_name_valid(value)
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.ends_with('/')
        || value.ends_with('.')
    {
        return Err(invalid());
    }
    for part in value.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.starts_with('.')
            || part.ends_with(".lock")
        {
            return Err("Invalid Git branch name component".to_string());
        }
    }
    Ok(value)
}

pub fn canonical_branch_slug(branch_name: &str) -> Result<String, String> {
    let branch = validate_branch_name(branch_name)?;
    let digest16 = &sha256_hex(branch.as_bytes())[..16];
    // re.sub(r'[^a-z0-9._-]+', '-', branch.lower())
    let lowered = branch.to_lowercase();
    let mut sanitized = String::new();
    let mut in_bad_run = false;
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            sanitized.push(c);
            in_bad_run = false;
        } else if !in_bad_run {
            sanitized.push('-');
            in_bad_run = true;
        }
    }
    // re.sub(r'-{2,}', '-', ...) then strip('._-')
    let mut collapsed = String::new();
    let mut last_dash = false;
    for c in sanitized.chars() {
        if c == '-' {
            if !last_dash {
                collapsed.push(c);
            }
            last_dash = true;
        } else {
            collapsed.push(c);
            last_dash = false;
        }
    }
    let stripped = collapsed.trim_matches(|c| matches!(c, '.' | '_' | '-'));
    let mut prefix: String = stripped.chars().take(40).collect();
    prefix = prefix
        .trim_end_matches(|c| matches!(c, '.' | '_' | '-'))
        .to_string();
    if prefix.is_empty() || !prefix.chars().next().unwrap().is_alphanumeric() {
        prefix = "b".to_string();
    }
    let slug = format!("{prefix}-{digest16}");
    if !branch_slug_valid(&slug) {
        return Err("Canonical branch slug failed validation".to_string());
    }
    Ok(slug)
}

fn commits_valid(candidate_commit: &str, expected_main: &str) -> bool {
    is_commit(&json!(candidate_commit)) && is_commit(&json!(expected_main))
}

pub fn derive_branch_scope(
    candidate_commit: &str,
    expected_main: &str,
    source_branch: Option<&str>,
) -> Result<String, String> {
    if !commits_valid(candidate_commit, expected_main) {
        return Err("Invalid commit identity for branch scope derivation".to_string());
    }
    let Some(branch) = source_branch else {
        return Ok(if candidate_commit == expected_main {
            "main".to_string()
        } else {
            "detached".to_string()
        });
    };
    let branch = validate_branch_name(branch)?;
    if branch == "main" {
        if candidate_commit != expected_main {
            return Err("Non-main candidate cannot claim main branch scope".to_string());
        }
        return Ok("main".to_string());
    }
    Ok(format!("branch/{}", canonical_branch_slug(branch)?))
}

pub fn validate_branch_scope(
    scope: &str,
    candidate_commit: &str,
    expected_main: &str,
) -> Result<String, String> {
    if !commits_valid(candidate_commit, expected_main) {
        return Err("Invalid commit identity for branch scope validation".to_string());
    }
    if scope == "main" {
        if candidate_commit != expected_main {
            return Err("Main branch scope requires candidate_commit == expected_main".to_string());
        }
        return Ok("main".to_string());
    }
    if scope == "detached" {
        return Ok("detached".to_string());
    }
    if !scope.contains("..")
        && scope
            .strip_prefix("branch/")
            .map(branch_slug_valid)
            .unwrap_or(false)
    {
        return Ok(scope.to_string());
    }
    Err("Invalid workflow branch scope".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_scope_match_python() {
        // Pinned against work_external_workflow.canonical_branch_slug.
        assert_eq!(
            canonical_branch_slug("Feature/Fix_Thing.Now").unwrap(),
            "feature-fix_thing.now-c666980f678e8970"
        );
        assert_eq!(
            derive_branch_scope(&"c".repeat(40), &"d".repeat(40), Some("Feature/Fix_Thing.Now"))
                .unwrap(),
            "branch/feature-fix_thing.now-c666980f678e8970"
        );
        assert_eq!(
            derive_branch_scope(&"d".repeat(40), &"d".repeat(40), None).unwrap(),
            "main"
        );
        assert_eq!(
            derive_branch_scope(&"c".repeat(40), &"d".repeat(40), None).unwrap(),
            "detached"
        );
        assert!(derive_branch_scope(&"c".repeat(40), &"d".repeat(40), Some("main")).is_err());
        assert_eq!(
            validate_branch_scope("main", &"d".repeat(40), &"d".repeat(40)).unwrap(),
            "main"
        );
        assert!(validate_branch_scope("branch/bad..slug", &"c".repeat(40), &"d".repeat(40)).is_err());
        assert!(validate_branch_name("bad//name").is_err());
        assert!(validate_branch_name("-leading").is_err());
        assert!(validate_branch_name("ok/name.lock").is_err());
    }
}
