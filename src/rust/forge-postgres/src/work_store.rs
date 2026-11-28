use crate::factory::PgSessionFactory;
use crate::work_common;
use forge_core::provider::Factory;

/// The review/repair policy the store upserts for each project,
/// mirroring Python's DEFAULT_REPAIR_POLICY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPolicy {
    /// The policy version string.
    pub version: String,
    /// The maximum repair rounds per root task.
    pub max_rounds: i64,
    /// The maximum stagnant rounds per root task.
    pub max_stagnant_rounds: i64,
    /// The maximum infrastructure retries per root task.
    pub max_infrastructure_retries: i64,
    /// The hosted checks a publication requires green.
    pub required_hosted_checks: Vec<String>,
}

impl ReviewPolicy {
    /// Python's defaults: forge-repair-v1 with every limit at
    /// AGENT_RETRY_LIMIT (20) and the standard hosted checks.
    pub fn agent_defaults() -> ReviewPolicy {
        ReviewPolicy {
            version: "forge-repair-v1".to_string(),
            max_rounds: 20,
            max_stagnant_rounds: 20,
            max_infrastructure_retries: 20,
            required_hosted_checks: vec![
                "check".to_string(),
                "pr-fast-gate".to_string(),
                "real-browser".to_string(),
            ],
        }
    }
}

/// The forge-postgres adapter implementing forge-work's three driven
/// ports (the impls live in the work_read / work_admission /
/// work_decision modules and their delegation targets). Holds the
/// session factory, the owner scope, the allowed projects, the pending
/// cap, and the review policy - the WorkStore's configuration, with no
/// behavior of its own beyond session acquisition.
pub struct PgWorkStore {
    pub(crate) delivery_enabled: bool,
    pub(crate) recipes: Vec<forge_work::pinned_recipe::PinnedRecipe>,
    /// The factory fresh sessions are created through, per operation.
    pub(crate) factory: PgSessionFactory,
    /// The owner scope every query binds.
    pub(crate) owner: String,
    /// The projects this store may touch (require_project's allowlist).
    pub(crate) allowed_projects: Vec<String>,
    /// The per-project pending cap for submit/resume.
    pub(crate) max_pending: i64,
    /// The review policy upserted by ensure_review_policy.
    pub(crate) repair_policy: ReviewPolicy,
}

impl PgWorkStore {
    /// Builds the store from its configuration parts.
    pub fn new(
        factory: PgSessionFactory,
        owner: impl Into<String>,
        allowed_projects: Vec<String>,
        max_pending: i64,
        repair_policy: ReviewPolicy,
    ) -> PgWorkStore {
        PgWorkStore {
            delivery_enabled: false,
            recipes: Vec::new(),
            factory,
            owner: owner.into(),
            allowed_projects,
            max_pending,
            repair_policy,
        }
    }

    /// Bind the validated current catalog without requiring a previous task to seed a recipe.
    pub fn with_recipe_catalog(
        mut self,
        config: &forge_work::work_config::WorkApiConfig,
    ) -> Result<Self, forge_work::WorkError> {
        if config.owner != self.owner || config.allowed_projects != self.allowed_projects {
            return Err(forge_work::WorkError::store(
                "Recipe catalog scope mismatch",
            ));
        }
        self.recipes = config
            .profiles
            .iter()
            .flat_map(|profile| {
                config
                    .allowed_modes
                    .iter()
                    .filter_map(|mode| config.resolve_recipe(&profile.alias, mode))
            })
            .collect();
        Ok(self)
    }

    pub fn with_delivery_enabled(mut self, enabled: bool) -> Self {
        self.delivery_enabled = enabled;
        self
    }

    /// Creates a fresh session through the factory (a transient session
    /// per operation, the pattern the worker/reconciler surfaces use).
    pub(crate) fn session(&self) -> Result<crate::client::PgSession, forge_work::WorkError> {
        self.factory.create().map_err(work_common::work_err)
    }

    /// The owner scope every query binds.
    pub fn owner(&self) -> &str {
        &self.owner
    }
}
