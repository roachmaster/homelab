//! The Studio build worker: claims a queued build, builds and
//! publishes its artifact, and finishes the attempt. The store and the
//! artifact pipeline are ports bound by the composition root.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::artifact;
use crate::service::{Catalog, PROFILE};
use crate::store::{BuildStore, StoreError};

/// Builds a project revision and publishes the resulting artifact.
pub trait ArtifactPipeline: Send + Sync {
    fn build(&self, repository: &Path, revision: &str, directory: &str) -> Result<Vec<u8>, String>;
    fn publish(&self, content: &[u8]) -> Result<(String, i64), String>;
}

/// Production pipeline: `git archive` builds, published into the
/// artifact directory.
pub struct GitArtifacts {
    directory: PathBuf,
}

impl GitArtifacts {
    pub fn new(directory: PathBuf) -> GitArtifacts {
        GitArtifacts { directory }
    }
}

impl ArtifactPipeline for GitArtifacts {
    fn build(&self, repository: &Path, revision: &str, directory: &str) -> Result<Vec<u8>, String> {
        artifact::build(repository, revision, directory)
    }

    fn publish(&self, content: &[u8]) -> Result<(String, i64), String> {
        artifact::publish(&self.directory, content)
    }
}

pub struct BuildWorker {
    store: Arc<dyn BuildStore>,
    catalog: Catalog,
    artifacts: Box<dyn ArtifactPipeline>,
}

impl BuildWorker {
    pub fn new(
        store: Arc<dyn BuildStore>,
        catalog: Catalog,
        artifacts: Box<dyn ArtifactPipeline>,
    ) -> BuildWorker {
        BuildWorker {
            store,
            catalog,
            artifacts,
        }
    }

    /// Runs forever, idling as `run_once` asks.
    pub fn run(&self) -> ! {
        loop {
            if let Some(idle) = self.run_once() {
                std::thread::sleep(idle);
            }
        }
    }

    /// One claim attempt. Returns how long to idle before the next
    /// one: 2s after a database error, 1s after a rejection or an
    /// empty queue, none after handling a claim.
    pub fn run_once(&self) -> Option<Duration> {
        let claim = match self.store.claim() {
            Err(StoreError::Db(_)) => return Some(Duration::from_secs(2)),
            Err(_) | Ok(None) => return Some(Duration::from_secs(1)),
            Ok(Some(claim)) => claim,
        };
        let project = self
            .catalog
            .projects
            .get(&(claim.node_alias.clone(), claim.project_alias.clone()));
        let Some(project) = project else {
            let _ = self.store.finish(&claim, None);
            return None;
        };
        if claim.profile != PROFILE || claim.source_recipe != project.source_recipe() {
            let _ = self.store.finish(&claim, None);
            return None;
        }
        match self
            .artifacts
            .build(&project.repository, &claim.revision, &project.directory)
        {
            // Publish only while the lease is still ours; a lost lease
            // leaves the attempt to its new holder.
            Ok(content) => {
                if let Ok(true) = self.store.renew(&claim) {
                    let published = self.artifacts.publish(&content);
                    let artifact = published.as_ref().ok();
                    let _ = self.store.finish(
                        &claim,
                        artifact.map(|(digest, size)| (digest.as_str(), *size)),
                    );
                }
            }
            Err(_) => {
                let _ = self.store.finish(&claim, None);
            }
        }
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::service::Project;
    use crate::store::{BuildRow, StoreResult};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Test-env store: serves scripted claims, records finishes.
    #[derive(Default)]
    pub(crate) struct FakeStore {
        pub claims: Mutex<Vec<StoreResult<Option<BuildRow>>>>,
        pub renew: Mutex<Option<bool>>,
        pub finished: Mutex<Vec<Option<(String, i64)>>>,
        pub submitted: Mutex<Vec<String>>,
    }

    impl BuildStore for FakeStore {
        fn submit(
            &self,
            _project: &(String, String),
            revision: &str,
            _profile: &str,
            _key: &str,
            _max_builds: i32,
            _approved_revision: &str,
            _source_recipe: &str,
        ) -> StoreResult<BuildRow> {
            self.submitted.lock().unwrap().push(revision.to_string());
            Ok(row(revision))
        }
        fn status(&self, _: &(String, String), _: &str, _: i32) -> StoreResult<BuildRow> {
            Ok(row(&"a".repeat(40)))
        }
        fn cancel(&self, _: &(String, String), _: &str) -> StoreResult<BuildRow> {
            Ok(row(&"a".repeat(40)))
        }
        fn claim(&self) -> StoreResult<Option<BuildRow>> {
            self.claims.lock().unwrap().pop().unwrap_or(Ok(None))
        }
        fn renew(&self, _: &BuildRow) -> StoreResult<bool> {
            Ok(self.renew.lock().unwrap().unwrap_or(true))
        }
        fn finish(&self, _: &BuildRow, artifact: Option<(&str, i64)>) -> StoreResult<bool> {
            self.finished
                .lock()
                .unwrap()
                .push(artifact.map(|(digest, size)| (digest.to_string(), size)));
            Ok(true)
        }
    }

    struct FakeArtifacts {
        build: Result<Vec<u8>, String>,
        publish: Result<(String, i64), String>,
    }

    impl ArtifactPipeline for FakeArtifacts {
        fn build(&self, _: &Path, _: &str, _: &str) -> Result<Vec<u8>, String> {
            self.build.clone()
        }
        fn publish(&self, _: &[u8]) -> Result<(String, i64), String> {
            self.publish.clone()
        }
    }

    pub(crate) fn project() -> Project {
        Project {
            node: "node".to_string(),
            project: "game".to_string(),
            revision: "a".repeat(40),
            repository: PathBuf::from("/srv/repo"),
            directory: "web".to_string(),
        }
    }

    pub(crate) fn catalog() -> Catalog {
        let mut projects = BTreeMap::new();
        projects.insert(("node".to_string(), "game".to_string()), project());
        Catalog {
            projects,
            max_builds: 8,
        }
    }

    pub(crate) fn row(revision: &str) -> BuildRow {
        BuildRow {
            build_id: "12345678-1234-1234-1234-123456789012".to_string(),
            node_alias: "node".to_string(),
            project_alias: "game".to_string(),
            revision: revision.to_string(),
            profile: PROFILE.to_string(),
            source_recipe: project().source_recipe(),
            intent_key: "key".to_string(),
            state: "queued".to_string(),
            attempts: 1,
            attempt_token: None,
            artifact_digest: None,
            artifact_bytes: None,
            events: Vec::new(),
        }
    }

    fn worker(
        store: &Arc<FakeStore>,
        build: Result<Vec<u8>, String>,
        publish: Result<(String, i64), String>,
    ) -> BuildWorker {
        BuildWorker::new(
            Arc::clone(store) as Arc<dyn BuildStore>,
            catalog(),
            Box::new(FakeArtifacts { build, publish }),
        )
    }

    fn claim(store: &FakeStore, claim: StoreResult<Option<BuildRow>>) {
        store.claims.lock().unwrap().push(claim);
    }

    fn ok_worker(store: &Arc<FakeStore>) -> BuildWorker {
        worker(store, Ok(b"zip".to_vec()), Ok(("digest".to_string(), 3)))
    }

    #[test]
    fn idle_cadence_follows_the_claim_outcome() {
        let store = Arc::new(FakeStore::default());
        let worker = ok_worker(&store);
        claim(&store, Err(StoreError::Db("down".to_string())));
        assert_eq!(worker.run_once(), Some(Duration::from_secs(2)));
        claim(&store, Err(StoreError::rejected(409, "busy")));
        assert_eq!(worker.run_once(), Some(Duration::from_secs(1)));
        assert_eq!(
            worker.run_once(),
            Some(Duration::from_secs(1)),
            "empty queue"
        );
        assert!(store.finished.lock().unwrap().is_empty());
    }

    #[test]
    fn a_built_and_published_claim_finishes_with_its_artifact() {
        let store = Arc::new(FakeStore::default());
        claim(&store, Ok(Some(row(&"a".repeat(40)))));
        assert_eq!(ok_worker(&store).run_once(), None);
        assert_eq!(
            *store.finished.lock().unwrap(),
            vec![Some(("digest".to_string(), 3))]
        );
    }

    #[test]
    fn unknown_projects_and_stale_recipes_fail_the_attempt() {
        let store = Arc::new(FakeStore::default());
        let mut unknown = row(&"a".repeat(40));
        unknown.project_alias = "other".to_string();
        claim(&store, Ok(Some(unknown)));
        let mut stale = row(&"a".repeat(40));
        stale.source_recipe = "old".to_string();
        claim(&store, Ok(Some(stale)));
        let worker = ok_worker(&store);
        assert_eq!(worker.run_once(), None);
        assert_eq!(worker.run_once(), None);
        assert_eq!(*store.finished.lock().unwrap(), vec![None, None]);
    }

    #[test]
    fn build_or_publish_failures_fail_the_attempt() {
        let store = Arc::new(FakeStore::default());
        claim(&store, Ok(Some(row(&"a".repeat(40)))));
        claim(&store, Ok(Some(row(&"a".repeat(40)))));
        worker(&store, Err("git".to_string()), Ok(("d".to_string(), 1))).run_once();
        worker(&store, Ok(b"zip".to_vec()), Err("disk".to_string())).run_once();
        assert_eq!(*store.finished.lock().unwrap(), vec![None, None]);
    }

    #[test]
    fn a_lost_lease_leaves_the_attempt_unfinished() {
        let store = Arc::new(FakeStore::default());
        *store.renew.lock().unwrap() = Some(false);
        claim(&store, Ok(Some(row(&"a".repeat(40)))));
        assert_eq!(ok_worker(&store).run_once(), None);
        assert!(store.finished.lock().unwrap().is_empty());
    }
}
