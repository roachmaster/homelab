//! Effect-free host factory registration. Files, accounts, signals and processes
//! are discovered only by the constructed host's explicit run operation.
use crate::{Config, NativeProcess, VerificationHost};
use forge_core::bootstrap::PreparedGroup;
use forge_core::provider::Factory;

#[derive(Debug, Clone)]
pub struct HostFactory {
    config: Config,
}
impl Factory<VerificationHost<NativeProcess>> for HostFactory {
    type Error = String;
    fn create(&self) -> Result<VerificationHost<NativeProcess>, String> {
        Ok(VerificationHost::new(self.config.clone(), NativeProcess))
    }
}
pub fn register(config: Config) -> Result<PreparedGroup<HostFactory>, String> {
    config.validate()?;
    PreparedGroup::new(
        "forge.ci_verifier.host",
        &["forge.ci_verifier.host.factory"],
        &[],
        HostFactory { config },
    )
    .map_err(|e| e.to_string())
}
