use crate::catalog::{ProjectCatalog, WorkCatalog};
use crate::work_error::WorkError;
use forge_core::bootstrap::{PreparedGroup, RegistrationError};
use forge_core::provider::Factory;

/// The catalog factory: creates the frozen WorkCatalog from the project
/// values staged at registration.
pub struct WorkCatalogFactory {
    /// The staged project catalogs.
    projects: Vec<ProjectCatalog>,
}

impl WorkCatalogFactory {
    /// Stages the project values the catalog will be built from.
    pub fn new(projects: Vec<ProjectCatalog>) -> WorkCatalogFactory {
        WorkCatalogFactory { projects }
    }
}

impl Factory<WorkCatalog> for WorkCatalogFactory {
    type Error = WorkError;

    /// Builds the frozen catalog from the staged projects.
    fn create(&self) -> Result<WorkCatalog, Self::Error> {
        Ok(WorkCatalog::new(self.projects.clone()))
    }
}

/// Stages the work catalog as a bootstrap group (id forge.work.catalog,
/// provides work-catalog). The registrar receives explicit values and
/// reads no environment; the catalog is created only when the frozen
/// registry is consumed.
pub fn register_catalog(
    projects: Vec<ProjectCatalog>,
) -> Result<PreparedGroup<WorkCatalogFactory>, RegistrationError> {
    PreparedGroup::new(
        "forge.work.catalog",
        &["work-catalog"],
        &[],
        WorkCatalogFactory::new(projects),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AgentProfile;

    fn projects() -> Vec<ProjectCatalog> {
        vec![ProjectCatalog {
            alias: "homelab".to_string(),
            profiles: vec![AgentProfile {
                name: "muse-db-primary".to_string(),
                backend: "muse".to_string(),
                model: "muse".to_string(),
                transport: "db".to_string(),
                modes: vec!["plan".to_string()],
            }],
            recipes: vec![],
            workflow: None,
        }]
    }

    #[test]
    fn factory_creates_catalog_with_staged_projects() {
        let factory = WorkCatalogFactory::new(projects());
        let catalog = factory.create().expect("create");
        assert!(catalog.profile("homelab", "muse-db-primary").is_some());
        assert!(catalog.project("other").is_none());
    }

    #[test]
    fn registrar_stages_the_group() {
        assert!(register_catalog(projects()).is_ok());
    }
}
