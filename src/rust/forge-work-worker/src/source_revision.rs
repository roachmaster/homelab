//! Explicit source selection from durable merge receipts
//! and a private Git ref (the SelectedSourceRevisionProvider
//! half of work_source_revision.py, assigned to S2 in the
//! migration map but unported there; the publisher half
//! lives in forge-work-publication).
//!
//! Coding-worker-scoped source reader; no registry, model
//! or network access. Every failure collapses to the single
//! fixed SourceRevisionError message, as in Python.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const VERIFIED_REF: &str = "refs/forge/verified-main";
pub const SOURCE_REVISION_MESSAGE: &str = "The configured verified project source is unavailable.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRevisionError;

impl std::fmt::Display for SourceRevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{SOURCE_REVISION_MESSAGE}")
    }
}

impl std::error::Error for SourceRevisionError {}

fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The store port: durable per-project verified revisions
/// (WorkStore.project_revisions; the forge-postgres adapter
/// already exposes it).
pub trait ProjectRevisionsPort: Send {
    fn project_revisions(&mut self) -> Result<BTreeMap<String, String>, String>;
}

/// The plain git call port (the worker's git executor):
/// raw stdout for one repository invocation.
pub trait SourceGitPort: Send {
    fn git(&mut self, repository: &Path, args: &[&str]) -> Result<Vec<u8>, String>;
}

fn private_revision(
    git: &mut dyn SourceGitPort,
    repository: &Path,
) -> Result<Option<String>, SourceRevisionError> {
    let raw = git
        .git(
            repository,
            &[
                "for-each-ref",
                "--count=2",
                "--format=%(refname) %(objectname)",
                VERIFIED_REF,
            ],
        )
        .map_err(|_| SourceRevisionError)?;
    let text = String::from_utf8_lossy(&raw);
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let values: Vec<&str> = text.split_whitespace().collect();
    if values.len() != 2 || values[0] != VERIFIED_REF || !is_sha(values[1]) {
        return Err(SourceRevisionError);
    }
    Ok(Some(values[1].to_string()))
}

pub struct SelectedSourceRevisionProvider {
    pub projects: Vec<(String, PathBuf)>,
    pub autonomous_projects: Vec<String>,
    pub store: Box<dyn ProjectRevisionsPort>,
    pub git: Box<dyn SourceGitPort>,
}

impl SelectedSourceRevisionProvider {
    pub fn projects(&mut self) -> Result<Vec<Value>, SourceRevisionError> {
        self.projects_inner().map_err(|_| SourceRevisionError)
    }

    fn projects_inner(&mut self) -> Result<Vec<Value>, String> {
        let known: BTreeMap<String, String> = if self.autonomous_projects.is_empty() {
            BTreeMap::new()
        } else {
            self.store.project_revisions()?
        };
        if known.len() > 64 || known.iter().any(|(_alias, head)| !is_sha(head)) {
            return Err(SOURCE_REVISION_MESSAGE.to_string());
        }
        let mut result = Vec::new();
        for (alias, repository) in &self.projects {
            let mut head = "HEAD".to_string();
            if self.autonomous_projects.iter().any(|item| item == alias) {
                let reference = private_revision(&mut *self.git, repository)
                    .map_err(|error| error.to_string())?;
                let expected = known.get(alias).cloned();
                if reference != expected {
                    return Err(SOURCE_REVISION_MESSAGE.to_string());
                }
                if let Some(expected) = expected {
                    head = expected;
                }
            }
            let raw = self.git.git(
                repository,
                &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
            )?;
            let revision = String::from_utf8_lossy(&raw).trim().to_string();
            if !is_sha(&revision) || (head != "HEAD" && head != revision) {
                return Err(SOURCE_REVISION_MESSAGE.to_string());
            }
            result.push(json!({"alias": alias, "base_revision": revision}));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeGit {
        verified: Option<String>,
    }

    impl SourceGitPort for FakeGit {
        fn git(&mut self, _repository: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
            if args[0] == "for-each-ref" {
                return Ok(match &self.verified {
                    Some(head) => format!("{VERIFIED_REF} {head}\n").into_bytes(),
                    None => Vec::new(),
                });
            }
            // rev-parse: echo the requested head back as the
            // resolved commit.
            let head = args[2].trim_end_matches("^{commit}");
            if head == "HEAD" {
                return Ok(format!("{}\n", "c".repeat(40)).into_bytes());
            }
            Ok(format!("{head}\n").into_bytes())
        }
    }

    struct FakeStore {
        revisions: BTreeMap<String, String>,
    }

    impl ProjectRevisionsPort for FakeStore {
        fn project_revisions(&mut self) -> Result<BTreeMap<String, String>, String> {
            Ok(self.revisions.clone())
        }
    }

    #[test]
    fn projects_resolve_head_and_verified() {
        let head = "a".repeat(40);
        let mut provider = SelectedSourceRevisionProvider {
            projects: vec![
                ("homelab".to_string(), PathBuf::from("/srv/homelab")),
                ("other".to_string(), PathBuf::from("/srv/other")),
            ],
            autonomous_projects: vec!["homelab".to_string()],
            store: Box::new(FakeStore {
                revisions: BTreeMap::from([("homelab".to_string(), head.clone())]),
            }),
            git: Box::new(FakeGit {
                verified: Some(head.clone()),
            }),
        };
        let result = provider.projects().unwrap();
        assert_eq!(result[0]["base_revision"], json!(head));
        assert_eq!(result[1]["base_revision"], json!("c".repeat(40)));
    }

    #[test]
    fn mismatched_verified_ref_is_the_fixed_error() {
        let mut provider = SelectedSourceRevisionProvider {
            projects: vec![("homelab".to_string(), PathBuf::from("/srv/homelab"))],
            autonomous_projects: vec!["homelab".to_string()],
            store: Box::new(FakeStore {
                revisions: BTreeMap::from([("homelab".to_string(), "a".repeat(40))]),
            }),
            git: Box::new(FakeGit {
                verified: Some("b".repeat(40)),
            }),
        };
        assert_eq!(provider.projects(), Err(SourceRevisionError));
    }
}
