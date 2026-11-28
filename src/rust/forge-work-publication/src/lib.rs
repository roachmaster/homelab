//! Publication and delivery for Forge work (the S4 slice:
//! work_ci_contract, work_verification, work_publication_*,
//! work_merge, work_post_merge, work_hosted_checks,
//! work_external_delivery, work_jenkins_lifecycle,
//! work_native_jenkins, work_delivery_*).
//!
//! Increment P1 (foundations): artifact/verification identities,
//! the closed v3 CI phase plan, the verification report
//! projection, the PID-1 verification launcher recipe, the
//! content identity contract and the read-only Jenkins observer.

pub mod artifact_identity;
pub mod artifact_source;
pub mod ci_contract;
pub mod cicd_artifacts;
pub mod ci_phases;
pub mod delivery_ops;
pub mod delivery_repair;
pub mod delivery_stores;
pub mod delivery_worker;
pub mod merge;
pub mod durable_files;
pub mod external_delivery;
pub mod external_identity;
pub mod external_verification;
pub mod identity_contract;
pub mod jenkins_observer;
pub mod hosted_checks;
pub mod identity_store;
pub mod native_identity_provider;
pub mod native_ci_proofs;
pub mod native_jenkins;
pub mod native_repair;
pub mod post_merge;
pub mod pipeline_projection;
pub mod publication_checks;
pub mod publication_clients;
pub mod publication;
pub mod publication_config;
pub mod jenkins_lifecycle;
pub mod post_merge_provider;
pub mod source_revision;
pub mod strict_json;
pub mod trusted_delivery;
pub mod trusted_delivery_provider;
pub mod verification;
pub mod verification_init;
pub mod verifier_parity;
