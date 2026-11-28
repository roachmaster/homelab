//! Production composition for the publication worker host
//! (work_publication.py's PublicationWorkerFactory and
//! default_store_factory).
//!
//! Python's factory shares single objects (store facade, git,
//! candidates, native verification, branch provider) between the
//! worker and its sub-workers. The Rust ports are stateless
//! handles over the same durable state (PostgreSQL rows, durable
//! files, the identity store), so the composition builds one
//! handle per consumer over the same configuration — with the one
//! exception of the native verification, whose in-memory
//! accounting is shared through SharedNativeCi exactly as Python
//! shares it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use forge_jenkins::api::{create_jenkins_service, JenkinsEventsList, JenkinsService};
use forge_jenkins::config::{load_jenkins_config, JenkinsConfig};
use forge_jenkins::emitter::InProcessJenkinsEmitter;
use forge_jenkins::events::JenkinsEventEmit;
use forge_jenkins::store::JenkinsStorePort;
use forge_work_worker::agent_branch::{AgentBranchProvider, PERSISTENT_BRANCH_POLICY};
use forge_work_worker::config::WorkerConfig;
use forge_work_worker::consumer_cache::configured_consumer_caches;
use forge_work_worker::native_identity::NativeIdentityConfig;
use forge_work_worker::ports_runtime::{SupervisorPort, SystemCommandPort};
use forge_work_worker::review_candidate::ReviewedCandidateProvider;
use forge_work_worker::review_ports::{
    ChecksPort, ContentIdentityPort, HostedChecksPort, NativeCiPort, RepairSourcesPort,
};
use forge_work_worker::review_worker::{ReviewConfig, ReviewWorker};
use forge_work_worker::supervisor::SystemdSupervisor;

use forge_work_publication::artifact_source::{
    CallableGitExecutor, GitArtifactGitPort, SourceLimits,
};
use forge_work_publication::delivery_ops::{
    create_release_provider, RootActorPort, SourceObserver,
};
use forge_work_publication::delivery_stores::{
    DeliveryStorePort, DeliveryVerificationStore, MergeStorePort, PostMergeStorePort,
};
use forge_work_publication::delivery_worker::ReleaseWorker;
use forge_work_publication::durable_files::{effective_uid, read_private};
use forge_work_publication::external_identity::GitArtifactsProvider;
use forge_work_publication::hosted_checks::GitHubHostedChecksProvider;
use forge_work_publication::jenkins_lifecycle::{IntentDrain, JenkinsEventEmitter};
use forge_work_publication::merge::{
    GitHubExactTreeMergeProvider, MergeBranchPort, MergeWorker, NoMergeRepair, PassthroughNativeCi,
    SharedBranches,
};
use forge_work_publication::native_identity_provider::{
    NativeVerificationIdentityFactory, NativeVerificationIdentityProvider,
};
use forge_work_publication::native_jenkins::{
    create_native_jenkins, BranchProviderObject, NativeJenkinsOptions, SharedAgentBranches,
    SharedEventLookup, SharedNativeCi,
};
use forge_work_publication::native_repair::BranchFirstRepairProvider;
use forge_work_publication::post_merge::PostMergeWorker;
use forge_work_publication::post_merge_provider::PostMergeProviderFactory;
use forge_work_publication::publication::{PublicationCollaborators, PublicationWorker};
use forge_work_publication::publication_checks::IsolatedCheckRunner;
use forge_work_publication::publication_clients::ProviderError;
use forge_work_publication::publication_clients::{
    github_remote_urls, CodingWorkspacesAdapter, CommandExecutor, GitHubPullRequestClient,
    GitRepositoryProvider, NoBranchRecords, ProcessCommandExecutor, PublicationBranchPort,
    SharedSnapshots, WorkspaceSnapshotProvider,
};
use forge_work_publication::publication_config::PublicationConfig;
use forge_work_publication::source_revision::VerifiedSourceSyncProvider;
use forge_work_publication::trusted_delivery::{DeliveryConfig, JenkinsObserverPort};
use forge_work_publication::trusted_delivery_provider::DeliveryProviderFactory;

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::delivery_workers_adapter::PgDeliveryWorkers;
use crate::external_delivery_adapter::PgBranchProvider;
use crate::factory::PgSessionFactory;
use crate::jenkins_store_adapter::PgJenkinsStore;
use crate::publication_store_adapter::{
    PgNativeCiStore, PgPublicationStore, RetryingPublicationListener,
};
use crate::review_store::PgReviewStore;
use crate::worker_composition::{passfile_password, pg_config_for};

type FactoryResult<T> = Result<T, String>;

fn executor() -> Box<dyn CommandExecutor> {
    Box::new(ProcessCommandExecutor)
}

fn provider_string(error: ProviderError) -> String {
    match error {
        ProviderError::Publication(publication) => publication.to_string(),
        ProviderError::Other(message) => message,
    }
}

/// The systemd supervisor behind a mutex so it satisfies the
/// publication ports' Send bound (Python shares one supervisor
/// object the same way).
struct SharedSupervisor(Mutex<SystemdSupervisor>);

impl SupervisorPort for SharedSupervisor {
    fn enforces_coding_sandbox(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .enforces_coding_sandbox()
    }

    fn argv(
        &mut self,
        lease: &forge_work_worker::Lease,
        invocation: &forge_work_worker::types::Invocation,
    ) -> Result<Vec<String>, forge_work_worker::error::WorkerError> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .argv(lease, invocation)
    }

    fn proof(
        &mut self,
        unit_name: &str,
    ) -> Result<forge_work_worker::types::StopProof, forge_work_worker::error::WorkerError> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .proof(unit_name)
    }

    fn stop(
        &mut self,
        unit_name: &str,
    ) -> Result<forge_work_worker::types::StopProof, forge_work_worker::error::WorkerError> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stop(unit_name)
    }
}

fn supervisor(worker: &WorkerConfig) -> FactoryResult<Box<dyn SupervisorPort + Send>> {
    let caches = configured_consumer_caches(worker).map_err(|error| error.to_string())?;
    Ok(Box::new(SharedSupervisor(Mutex::new(
        SystemdSupervisor::new(
            worker.clone(),
            Box::new(SystemCommandPort),
            Some(Box::new(caches)),
        ),
    ))))
}

/// Bounded raw git for the artifact/delivery ports (the host's
/// _git with an explicit timeout).
fn git_bytes_timeout(repo: &Path, args: &[&str], timeout_seconds: f64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut child = std::process::Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let stdout_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut bytes);
        bytes
    });
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stderr_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut bytes);
        bytes
    });
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_seconds.max(0.0));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout_thread.join().unwrap_or_default();
                let stderr = stderr_thread.join().unwrap_or_default();
                if status.success() {
                    return Ok(stdout);
                }
                return Err(format!(
                    "git exited with {status}: {}",
                    String::from_utf8_lossy(&stderr)
                ));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("git timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn git_port() -> Arc<dyn GitArtifactGitPort + Send + Sync> {
    Arc::new(CallableGitExecutor {
        execute: |repo: &Path, args: &[&str], timeout: f64| git_bytes_timeout(repo, args, timeout),
    })
}

fn projects_map(config: &PublicationConfig) -> BTreeMap<String, PathBuf> {
    config
        .worker
        .projects
        .iter()
        .map(|(alias, path)| (alias.clone(), path.clone()))
        .collect()
}

fn make_identity_provider(
    config: &PublicationConfig,
) -> FactoryResult<Option<NativeVerificationIdentityProvider>> {
    if config.hosted_provider != "jenkins" || config.native_identity_protocol != "content-v2" {
        return Ok(None);
    }
    let identity_config = NativeIdentityConfig::new(
        config.state_root(),
        &config.native_identity_protocol,
        config.native_identity_after.clone(),
    )?;
    let factory = NativeVerificationIdentityFactory {
        config: identity_config,
        owner: effective_uid(),
    };
    Ok(Some(factory.create(Box::new(GitArtifactsProvider {
        git: git_port(),
        limits: SourceLimits::default(),
    }))))
}

fn make_candidates(config: &PublicationConfig) -> FactoryResult<ReviewedCandidateProvider> {
    let identities = make_identity_provider(config)?
        .map(|provider| Box::new(provider) as Box<dyn ContentIdentityPort>);
    Ok(ReviewedCandidateProvider::new(
        config.state_root(),
        json!(config.checks),
        json!(config.required_hosted_checks),
        config.jenkins_verification_profile.clone(),
        projects_map(config),
        Box::new(make_git(config, Box::new(NoBranchRecords))),
        identities,
    ))
}

fn make_git(
    config: &PublicationConfig,
    branch_provider: Box<dyn PublicationBranchPort>,
) -> GitRepositoryProvider {
    GitRepositoryProvider::new(
        config.clone(),
        executor(),
        github_remote_urls(config),
        branch_provider,
    )
}

fn make_branch_provider(
    config: &PublicationConfig,
    pg: &PgConfig,
) -> FactoryResult<Option<AgentBranchProvider<PgBranchProvider>>> {
    if config.branch_policy != PERSISTENT_BRANCH_POLICY {
        return Ok(None);
    }
    let policy = config.resolved_branch_policy()?;
    let session = PgSession::connect(pg.clone()).map_err(|error| error.to_string())?;
    Ok(Some(AgentBranchProvider::new(
        policy,
        PgBranchProvider::new(session),
    )))
}

/// The publication/native-CI branch port: the persistent agent-branch
/// provider when the policy selects it, otherwise the shared-branch
/// null object.
fn select_branch_provider(
    config: &PublicationConfig,
    pg: &PgConfig,
) -> FactoryResult<Box<dyn BranchProviderObject>> {
    Ok(match make_branch_provider(config, pg)? {
        Some(provider) => Box::new(provider),
        None => Box::new(SharedAgentBranches),
    })
}

/// The factory constructor's frozen-selection checks
/// (PublicationWorkerFactory.__init__).
fn validate_factory_selection(
    config: &PublicationConfig,
    observer_present: bool,
) -> FactoryResult<()> {
    // NativeIdentityConfig construction validates the selection.
    let _ = NativeIdentityConfig::new(
        config.state_root(),
        &config.native_identity_protocol,
        config.native_identity_after.clone(),
    )?;
    if config.native_identity_protocol != "legacy-v1" && config.hosted_provider != "jenkins" {
        return Err("Content identity requires native Jenkins".to_string());
    }
    if config.merge_method != "merge"
        || (config.merge_enabled
            && !config
                .repositories
                .keys()
                .any(|alias| config.worker.autonomous_projects.contains(alias)))
    {
        return Err(
            "Trusted merge requires explicit verified project source selection".to_string(),
        );
    }
    if config.post_merge_enabled
        && (!config.merge_enabled
            || config.hosted_provider != "jenkins"
            || config.post_merge_jenkins_delivery_config.is_none())
    {
        return Err(
            "Post-merge verification requires explicit trusted Jenkins selection".to_string(),
        );
    }
    if observer_present && config.hosted_provider != "jenkins" {
        return Err("A Jenkins observer requires a selected Jenkins delivery consumer".to_string());
    }
    if (config.delivery_enabled
        && (!config.post_merge_enabled
            || !["forge-sdlc-jenkins-v2", "forge-sdlc-jenkins-v3"]
                .contains(&config.jenkins_verification_profile.as_str())))
        || ((config.release_enabled || config.source_observation_enabled)
            && !config.delivery_enabled)
    {
        return Err(
            "Delivery capabilities require the unified scoped Jenkins consumer".to_string(),
        );
    }
    let digest_ok = config
        .release_actor_sha256
        .as_deref()
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
    if config.source_observation_interval_seconds != 60
        || (config.release_enabled
            && !(config.repositories.len() == 1 && config.repositories.contains_key("homelab")))
        || (config.release_enabled && !digest_ok)
        || (!config.release_enabled && config.release_actor_sha256.is_some())
    {
        return Err("Invalid frozen delivery capability configuration".to_string());
    }
    Ok(())
}

/// The emit-only Jenkins services adapter: publication's emitter
/// port over the in-process emitter.
struct ServiceEmitter(InProcessJenkinsEmitter<JenkinsService>);

impl JenkinsEventEmitter for ServiceEmitter {
    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<(), String> {
        self.0
            .emit(event, None)
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }
}

/// _jenkins_events: build the validated emit-only Jenkins
/// services for the lifecycle producer.
fn jenkins_events(
    config: &PublicationConfig,
    selected: &JenkinsConfig,
) -> FactoryResult<(
    Box<dyn JenkinsEventEmitter>,
    Arc<SharedEventLookup>,
    String,
    Box<dyn JenkinsEventEmitter>,
)> {
    let section = config
        .jenkins_events
        .as_ref()
        .expect("caller checks jenkins_events");
    if selected.owner != config.worker.owner {
        return Err("Jenkins events owner does not match publication owner".to_string());
    }
    if !selected.allowed_instances.contains_key(&section.instance) {
        return Err("Jenkins events instance is not allowed".to_string());
    }
    for alias in config.repositories.keys() {
        if !selected.allowed_projects.contains(alias) {
            return Err("Jenkins events project is not allowed".to_string());
        }
    }
    let delivery_path = config
        .jenkins_delivery_config
        .clone()
        .ok_or_else(|| "Jenkins delivery configuration is unavailable".to_string())?;
    let raw =
        read_private(&delivery_path, 65_536, Some(effective_uid())).map_err(|message| message)?;
    let raw: Value =
        serde_json::from_slice(&raw).map_err(|_| "Invalid delivery configuration".to_string())?;
    let delivery = DeliveryConfig::parse(&raw).map_err(|error| error.to_string())?;
    for alias in config.repositories.keys() {
        if !selected.is_job_allowed(alias, &delivery.jenkins_job) {
            return Err("Jenkins events job is not allowed".to_string());
        }
    }
    let database = &section.database;
    if database.proxy.is_some() {
        return Err(
            "database.proxy is retired: Rust workers connect directly through forge-postgres"
                .to_string(),
        );
    }
    let host = database.host.to_string_lossy().into_owned();
    let password = database.passfile.as_deref().and_then(|path| {
        passfile_password(path, &host, database.port, &database.name, &database.user)
    });
    let pg = PgConfig {
        host,
        port: database.port,
        dbname: database.name.clone(),
        user: database.user.clone(),
        password,
        owner: config.worker.owner.clone(),
        connect_timeout: Duration::from_secs(4),
        statement_timeout: Duration::from_secs(10),
    };
    let store_config = selected.clone();
    let store_factory: forge_jenkins::consumer::StoreFactory = Arc::new(move || {
        Box::new(PgJenkinsStore::new(store_config.clone(), pg.clone())) as Box<dyn JenkinsStorePort>
    });
    let mut build_service = || -> FactoryResult<JenkinsService> {
        create_jenkins_service(
            Some(selected.clone()),
            Arc::clone(&store_factory),
            None,
            &config.worker.worker_id,
            false,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Jenkins events configuration is unavailable".to_string())
    };
    let lookup_service = Arc::new(Mutex::new(build_service()?));
    let emitter = Box::new(ServiceEmitter(InProcessJenkinsEmitter::new(
        build_service()?
    )));
    let drain_emitter = Box::new(ServiceEmitter(InProcessJenkinsEmitter::new(
        build_service()?
    )));
    let event_lookup: Arc<SharedEventLookup> = Arc::new(
        move |project_alias: &str,
              job_name: &str,
              commit_sha: &str|
              -> Result<Vec<Value>, String> {
            let mut service = lookup_service
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = service
                .list_events(&JenkinsEventsList {
                    project_alias: project_alias.to_string(),
                    after_sequence: 0,
                    limit: 100,
                    job_name: Some(job_name.to_string()),
                    event_kind: None,
                    commit_sha: Some(commit_sha.to_string()),
                })
                .map_err(|error| error.to_string())?;
            Ok(result["events"].as_array().cloned().unwrap_or_default())
        },
    );
    Ok((
        emitter,
        event_lookup,
        section.instance.clone(),
        drain_emitter,
    ))
}

/// default_store_factory's handle set plus the factory's create():
/// build the fully composed publication worker.
pub fn create_publication_worker(
    config: &PublicationConfig,
    stop: Arc<AtomicBool>,
    observer: Option<Box<dyn JenkinsObserverPort>>,
) -> FactoryResult<PublicationWorker> {
    validate_factory_selection(config, observer.is_some())?;
    let state_root = config.state_root();
    if !state_root.exists() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&state_root)
            .map_err(|_| "publication_state_unavailable".to_string())?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::symlink_metadata(&state_root)
            .map_err(|_| "publication_state_unavailable".to_string())?;
        if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o077 != 0 {
            return Err("publication_state_unavailable".to_string());
        }
    }

    let workspaces = CodingWorkspacesAdapter::new(config.worker.clone());
    workspaces.prepare().map_err(|error| error.to_string())?;
    let snapshots = SharedSnapshots::new(WorkspaceSnapshotProvider::new(
        Box::new(workspaces),
        executor(),
    ));

    let pg = pg_config_for(&config.worker, true);
    let store = Arc::new(PgPublicationStore::new(pg.clone()));
    let delivery_store = Arc::new(PgDeliveryWorkers::new(pg.clone()));

    let git = make_git(
        config,
        match make_branch_provider(config, &pg)? {
            Some(provider) => Box::new(provider) as Box<dyn PublicationBranchPort>,
            None => Box::new(NoBranchRecords),
        },
    );
    let pulls = GitHubPullRequestClient::new(config.clone(), executor());

    let mut native_shared: Option<SharedNativeCi> = None;
    let mut jenkins_drain: Option<IntentDrain> = None;
    let mut repair_for_review: Option<Box<dyn RepairSourcesPort>> = None;
    let mut repair_for_merge: Option<Box<dyn forge_work_publication::merge::MergeRepairPort>> =
        None;
    let reviewer_checks: Box<dyn ChecksPort>;
    let worker_checks: Box<dyn ChecksPort>;
    let reviewer_hosted: Option<Box<dyn HostedChecksPort>>;
    let reviewer_native: Option<Box<dyn NativeCiPort>>;
    let merge_hosted: Box<dyn HostedChecksPort>;

    if config.hosted_provider == "jenkins" {
        let (emitter, event_lookup, instance, drain_emitter) = match &config.jenkins_events {
            Some(_) => {
                let selected = load_jenkins_config(Some(
                    config
                        .jenkins_events
                        .as_ref()
                        .expect("checked")
                        .config_path
                        .to_string_lossy()
                        .as_ref(),
                ))?
                .ok_or_else(|| "Jenkins events configuration is unavailable".to_string())?;
                let (emitter, lookup, instance, drain) = jenkins_events(config, &selected)?;
                (Some(emitter), Some(lookup), Some(instance), Some(drain))
            }
            None => (None, None, None, None),
        };
        let options = NativeJenkinsOptions {
            client: Some(GitHubPullRequestClient::new(config.clone(), executor())),
            stop: Some(Arc::clone(&stop)),
            emitter,
            event_lookup,
            jenkins_instance: instance,
            identity_provider: make_identity_provider(config)?,
            branch_provider: select_branch_provider(config, &pg)?,
            observer,
            jenkins_execute: executor(),
            execute: executor(),
            git_port: git_port(),
        };
        let native = create_native_jenkins(
            config,
            Box::new(PgNativeCiStore::new(pg.clone())),
            make_git(config, Box::new(NoBranchRecords)),
            make_candidates(config)?,
            supervisor(&config.worker)?,
            options,
        )?;
        let shared = SharedNativeCi::new(native);
        if let Some(drain_emitter) = drain_emitter {
            let actions = shared.lock().delivery.config.state_root.join("actions");
            jenkins_drain = Some(IntentDrain::new(actions, drain_emitter, 2.0));
        }
        if config.native_delivery_mode == "branch-first-v1" {
            let repair = || -> FactoryResult<BranchFirstRepairProvider> {
                Ok(BranchFirstRepairProvider::new(
                    config.clone(),
                    make_git(config, Box::new(NoBranchRecords)),
                    make_candidates(config)?,
                    Box::new(shared.clone()),
                ))
            };
            repair_for_review = Some(Box::new(repair()?));
            repair_for_merge = Some(Box::new(repair()?));
        }
        reviewer_checks = Box::new(shared.clone());
        worker_checks = Box::new(shared.clone());
        reviewer_hosted = Some(Box::new(shared.clone()));
        reviewer_native = Some(Box::new(shared.clone()));
        merge_hosted = Box::new(shared.clone());
        native_shared = Some(shared);
    } else {
        reviewer_checks = Box::new(IsolatedCheckRunner::new(
            config.clone(),
            supervisor(&config.worker)?,
        ));
        worker_checks = Box::new(IsolatedCheckRunner::new(
            config.clone(),
            supervisor(&config.worker)?,
        ));
        reviewer_hosted = Some(Box::new(GitHubHostedChecksProvider::new(
            config.clone(),
            executor(),
            executor(),
        )));
        reviewer_native = None;
        merge_hosted = Box::new(GitHubHostedChecksProvider::new(
            config.clone(),
            executor(),
            executor(),
        ));
    }

    let mut review_config = ReviewConfig::new(
        config.repositories.keys().cloned().collect(),
        config
            .required_hosted_checks
            .iter()
            .map(|row| json!([row[0], row[1], row[2]]))
            .collect(),
        json!(config.checks),
        state_root.clone(),
        projects_map(config),
    );
    review_config.jenkins_verification_profile = config.jenkins_verification_profile.clone();
    review_config.native_identity_protocol = config.native_identity_protocol.clone();
    review_config.native_delivery_mode = config.native_delivery_mode.clone();
    let reviewer = ReviewWorker::new(
        review_config,
        Arc::new(PgReviewStore::new(PgSessionFactory::new(pg.clone()))),
        Box::new(snapshots.clone()),
        Box::new(make_git(config, Box::new(NoBranchRecords))),
        reviewer_checks,
        make_candidates(config)?,
        reviewer_hosted,
        reviewer_native,
        repair_for_review,
        make_branch_provider(config, &pg)?.map(|provider| {
            Box::new(provider) as Box<dyn forge_work_worker::review_ports::ReviewBranchPort>
        }),
        Arc::clone(&stop),
    );

    let merger = if config.merge_enabled {
        let provider = GitHubExactTreeMergeProvider {
            config: config.clone(),
            client: GitHubPullRequestClient::new(config.clone(), executor()),
            git: make_git(config, Box::new(NoBranchRecords)),
            candidates: make_candidates(config)?,
            hosted: merge_hosted,
            now: Box::new(|| {
                let epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs_f64())
                    .unwrap_or(0.0);
                forge_work_publication::jenkins_lifecycle::iso_from_epoch(epoch)
            }),
        };
        let merge_store: Arc<dyn MergeStorePort> = delivery_store.clone();
        Some(MergeWorker {
            config: config.clone(),
            store: merge_store,
            provider: Box::new(provider),
            source_sync: Box::new(VerifiedSourceSyncProvider {
                config: config.clone(),
                git: make_git(config, Box::new(NoBranchRecords)),
            }),
            stop: Arc::clone(&stop),
            native_ci: match native_shared.as_ref() {
                Some(shared) => Box::new(shared.clone()),
                None => Box::new(PassthroughNativeCi),
            },
            repair_sources: repair_for_merge.unwrap_or_else(|| Box::new(NoMergeRepair)),
            branch_provider: match make_branch_provider(config, &pg)? {
                Some(provider) => Box::new(provider) as Box<dyn MergeBranchPort>,
                None => Box::new(SharedBranches),
            },
        })
    } else {
        None
    };

    let mut post_merge = None;
    let mut source_observer = None;
    let mut release = None;
    if config.post_merge_enabled {
        let available = if config.delivery_enabled {
            delivery_store.delivery_available(&config.worker.owner)
        } else {
            delivery_store.post_merge_available(&config.worker.owner)
        }
        .map_err(provider_string)?;
        if !available {
            return Err(
                "Post-merge schema and scoped worker grant must be provisioned".to_string(),
            );
        }
        let provider = PostMergeProviderFactory {
            config: config.clone(),
        }
        .create(executor(), executor(), git_port())
        .map_err(provider_string)?;
        if config.source_observation_enabled {
            // A second delivery port over the same validated
            // post-merge configuration (Python shares the
            // provider's port; the Rust port is stateless).
            let delivery_path = config
                .post_merge_jenkins_delivery_config
                .clone()
                .expect("validated when post-merge is enabled");
            let raw = read_private(&delivery_path, 65_536, Some(effective_uid()))
                .map_err(|message| message)?;
            let raw: Value = serde_json::from_slice(&raw)
                .map_err(|_| "Invalid delivery configuration".to_string())?;
            let mut selected = DeliveryConfig::parse(&raw).map_err(|error| error.to_string())?;
            selected.state_root = state_root.join("native-ci");
            let delivery = DeliveryProviderFactory {
                config: selected,
                observer: None,
            }
            .create(executor(), executor(), git_port())
            .map_err(|error| error.to_string())?;
            source_observer = Some(SourceObserver::new(
                config.clone(),
                Box::new(PgDeliveryWorkers::new(pg.clone())),
                delivery,
            )?);
        }
        let post_merge_store: Arc<dyn PostMergeStorePort> = if config.delivery_enabled {
            Arc::new(DeliveryVerificationStore {
                store: delivery_store.clone(),
            })
        } else {
            delivery_store.clone()
        };
        let mut worker =
            PostMergeWorker::new(config.clone(), post_merge_store, Box::new(provider))?;
        worker.stop = Arc::clone(&stop);
        post_merge = Some(worker);
    }
    if config.release_enabled {
        let actor = RootActorPort::new(
            config
                .release_actor_sha256
                .as_deref()
                .expect("validated when release is enabled"),
        )?;
        let provider = create_release_provider(config, Box::new(actor))?;
        let release_store: Arc<dyn DeliveryStorePort> = delivery_store.clone();
        let mut worker = ReleaseWorker::new(config.clone(), release_store, Box::new(provider))?;
        worker.stop = Arc::clone(&stop);
        release = Some(worker);
    }

    // Same fallible order as before: candidates, then the branch
    // provider (which opens a session), then the listener.
    let candidates = make_candidates(config)?;
    let collaborators = PublicationCollaborators {
        reviewer: Some(reviewer),
        merger,
        native_ci: native_shared,
        post_merge,
        source_observer,
        release,
        jenkins_drain,
        branch_provider: select_branch_provider(config, &pg)?,
        stop,
        listener: Some(Box::new(RetryingPublicationListener::new(pg))),
    };
    Ok(PublicationWorker::new(
        config.clone(),
        store,
        Box::new(snapshots),
        git,
        worker_checks,
        pulls,
        candidates,
        collaborators,
    ))
}

/// The publication host's config loader (work_publication_config's
/// load_config + load_worker): read the publication JSON, load the
/// referenced worker config, and parse both.
pub fn load_publication_config(path: &Path) -> FactoryResult<PublicationConfig> {
    let raw =
        std::fs::read(path).map_err(|_| "Unable to load publication configuration".to_string())?;
    if raw.len() > 65_536 {
        return Err("Unable to load publication configuration".to_string());
    }
    let value: Value = serde_json::from_slice(&raw)
        .map_err(|_| "Unable to load publication configuration".to_string())?;
    let worker_path = value["worker_config"]
        .as_str()
        .ok_or_else(|| "Unable to load publication configuration".to_string())?;
    let worker_raw = std::fs::read_to_string(worker_path)
        .map_err(|_| "Unable to load publication configuration".to_string())?;
    let worker_value: Value = serde_json::from_str(&worker_raw)
        .map_err(|_| "Unable to load publication configuration".to_string())?;
    let mut worker = forge_work_worker::config::parse_config(&worker_value)
        .map_err(|_| "Unable to load publication configuration".to_string())?;
    worker.source_path = Some(PathBuf::from(worker_path));
    forge_work_publication::publication_config::parse_config(
        &value,
        &worker,
        Some(path.to_path_buf()),
    )
}

/// Binding the publication worker group provides.
pub const PUBLICATION_WORKER: &str = "forge.postgres.publication_worker";

/// The staged publication worker: the validated config, waiting for
/// the host to bind the observer it created from the frozen observer
/// exec factory.
pub struct PublicationWorkerRecipe {
    config: PublicationConfig,
}

impl PublicationWorkerRecipe {
    /// Bind the ready collaborators and produce the worker factory.
    /// Still effect-free: no session, thread or socket until
    /// `create()`.
    pub fn bind(
        self,
        observer: Option<Box<dyn JenkinsObserverPort>>,
        stop: Arc<AtomicBool>,
    ) -> PublicationWorkerFactory {
        PublicationWorkerFactory {
            config: self.config,
            observer: Mutex::new(Some(observer)),
            stop,
        }
    }
}

/// Creates the one publication worker a host process runs. The bound
/// observer is moved into that worker, so a second `create()` is
/// refused rather than silently building a worker without it.
pub struct PublicationWorkerFactory {
    config: PublicationConfig,
    observer: Mutex<Option<Option<Box<dyn JenkinsObserverPort>>>>,
    stop: Arc<AtomicBool>,
}

impl forge_core::provider::Factory<PublicationWorker> for PublicationWorkerFactory {
    type Error = String;
    fn create(&self) -> FactoryResult<PublicationWorker> {
        let observer = self
            .observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| "The publication worker was already created".to_string())?;
        create_publication_worker(&self.config, Arc::clone(&self.stop), observer)
    }
}

/// Stage the publication worker group. It requires the publication
/// runtime's observer exec binding (which in turn requires the process
/// exec client), so a host missing either is rejected at freeze before
/// any database session opens.
pub fn register_publication_worker(
    config: PublicationConfig,
) -> forge_core::bootstrap::PreparedGroup<PublicationWorkerRecipe> {
    forge_core::bootstrap::PreparedGroup::new(
        PUBLICATION_WORKER,
        &[PUBLICATION_WORKER],
        &[forge_work_publication::registrar::OBSERVER_BINDING],
        PublicationWorkerRecipe { config },
    )
    .expect("fixed publication worker group identifiers are valid")
}

#[cfg(test)]
mod publication_worker_registry_tests {
    use super::*;
    use forge_core::bootstrap::{Registry, ValidationError};
    use forge_core::provider::Factory;
    use forge_work_publication::registrar::{register_publication, OBSERVER_BINDING};
    use forge_work_worker::process_exec::register_process_exec;

    fn config() -> PublicationConfig {
        let worker = forge_work_worker::config::parse_config(&serde_json::json!({
            "worker_id": "publication-1",
            "owner": "forge",
            "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
            "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
            "workspace_root": "/srv/forge/workspaces",
            "state_root": "/srv/forge/state",
            "projects": {"homelab": "/srv/repos/homelab"},
            "profiles": {"codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan"]}},
            "binaries": {"codex": "/usr/local/bin/codex"}
        }))
        .unwrap();
        forge_work_publication::publication_config::parse_config(
            &serde_json::json!({
                "worker_config": "/srv/worker.json",
                "repositories": {"homelab": "roachmaster/homelab"},
                "github_config_dir": "/home/forgeqa/.config/gh",
            }),
            &worker,
            None,
        )
        .unwrap()
    }

    #[test]
    fn a_worker_without_the_publication_runtime_is_rejected_at_freeze() {
        let Err(rejected) = Registry::new()
            .register(register_publication_worker(config()))
            .unwrap()
            .freeze()
        else {
            panic!("the worker group requires the observer exec binding");
        };
        assert_eq!(
            rejected.reason(),
            &ValidationError::MissingRequirement {
                group: PUBLICATION_WORKER.to_string(),
                binding: OBSERVER_BINDING.to_string(),
            }
        );
    }

    #[test]
    fn the_production_plan_freezes_and_binds_without_effects() {
        let frozen = Registry::new()
            .register(register_process_exec().unwrap())
            .unwrap()
            .register(register_publication().unwrap())
            .unwrap()
            .register(register_publication_worker(config()))
            .unwrap()
            .freeze();
        let Ok(frozen) = frozen else {
            panic!("process exec, publication runtime and worker form a complete plan");
        };
        let ((((), _exec), runtime), worker) = frozen.into_factories();
        assert!(runtime.observer_exec.create().is_ok());
        let stop = Arc::new(AtomicBool::new(false));
        // Binding opens nothing; only create() reaches PostgreSQL.
        let _factory = worker.bind(None, stop);
    }

    #[test]
    fn the_worker_factory_creates_at_most_one_worker() {
        let factory = PublicationWorkerRecipe { config: config() }
            .bind(None, Arc::new(AtomicBool::new(false)));
        // Take the bound observer slot as the first create() would.
        factory.observer.lock().unwrap().take();
        let Err(message) = factory.create() else {
            panic!("a second create must be refused");
        };
        assert_eq!(message, "The publication worker was already created");
    }
}
