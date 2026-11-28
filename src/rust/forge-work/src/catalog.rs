use serde_json::Value;

/// AgentProfile represents a configuration for an agent profile.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentProfile {
    pub name: String,
    pub backend: String,
    pub model: String,
    pub transport: String,
    pub modes: Vec<String>,
}

/// Recipe represents a configuration for a recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub id: String,
    pub digest: String,
}

/// WorkflowPolicy represents a configuration for a workflow policy.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowPolicy {
    pub digest: String,
    pub document: Value,
}

/// ProjectCatalog represents the catalog for a project.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCatalog {
    pub alias: String,
    pub profiles: Vec<AgentProfile>,
    pub recipes: Vec<Recipe>,
    pub workflow: Option<WorkflowPolicy>,
}

/// WorkCatalog represents the frozen projects/profiles/recipes/workflow-policy reference data.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkCatalog {
    projects: Vec<ProjectCatalog>,
}

impl WorkCatalog {
    /// new creates a new WorkCatalog with the given projects.
    pub fn new(projects: Vec<ProjectCatalog>) -> WorkCatalog {
        WorkCatalog { projects }
    }

    /// project returns the project catalog with the given alias, or None if not found.
    pub fn project(&self, alias: &str) -> Option<&ProjectCatalog> {
        self.projects.iter().find(|p| p.alias == alias)
    }

    /// profile returns the agent profile with the given name in the project with the given alias, or None if not found.
    pub fn profile(&self, alias: &str, name: &str) -> Option<&AgentProfile> {
        self.projects.iter().find_map(|p| {
            if p.alias == alias {
                p.profiles.iter().find(|a| a.name == name)
            } else {
                None
            }
        })
    }

    /// recipe returns the recipe with the given id in the project with the given alias, or None if not found.
    pub fn recipe(&self, alias: &str, id: &str) -> Option<&Recipe> {
        self.projects.iter().find_map(|p| {
            if p.alias == alias {
                p.recipes.iter().find(|r| r.id == id)
            } else {
                None
            }
        })
    }

    /// workflow_digest returns the digest of the workflow policy in the project with the given alias, or None if not found.
    pub fn workflow_digest(&self, alias: &str) -> Option<&str> {
        self.projects.iter().find_map(|p| {
            if p.alias == alias {
                p.workflow.as_ref().map(|w| w.digest.as_str())
            } else {
                None
            }
        })
    }

    /// projects returns a slice of all project catalogs in the catalog.
    pub fn projects(&self) -> &[ProjectCatalog] {
        &self.projects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_catalog() {
        let profile1 = AgentProfile {
            name: "profile1".to_string(),
            backend: "backend1".to_string(),
            model: "model1".to_string(),
            transport: "transport1".to_string(),
            modes: vec!["mode1".to_string()],
        };
        let profile2 = AgentProfile {
            name: "profile2".to_string(),
            backend: "backend2".to_string(),
            model: "model2".to_string(),
            transport: "transport2".to_string(),
            modes: vec!["mode2".to_string()],
        };
        let recipe1 = Recipe {
            id: "recipe1".to_string(),
            digest: "digest1".to_string(),
        };
        let recipe2 = Recipe {
            id: "recipe2".to_string(),
            digest: "digest2".to_string(),
        };
        let workflow = WorkflowPolicy {
            digest: "workflow_digest".to_string(),
            document: serde_json::json!({}),
        };
        let project1 = ProjectCatalog {
            alias: "project1".to_string(),
            profiles: vec![profile1.clone()],
            recipes: vec![recipe1.clone()],
            workflow: Some(workflow.clone()),
        };
        let project2 = ProjectCatalog {
            alias: "project2".to_string(),
            profiles: vec![profile2.clone()],
            recipes: vec![recipe2.clone()],
            workflow: None,
        };
        let catalog = WorkCatalog::new(vec![project1.clone(), project2.clone()]);

        assert_eq!(catalog.project("project1"), Some(&project1));
        assert_eq!(catalog.project("project3"), None);
        assert_eq!(catalog.profile("project1", "profile1"), Some(&profile1));
        assert_eq!(catalog.profile("project1", "profile2"), None);
        assert_eq!(catalog.recipe("project1", "recipe1"), Some(&recipe1));
        assert_eq!(catalog.recipe("project1", "recipe2"), None);
        assert_eq!(catalog.workflow_digest("project1"), Some("workflow_digest"));
        assert_eq!(catalog.workflow_digest("project2"), None);
        assert_eq!(catalog.projects(), &[project1, project2]);
    }
}
