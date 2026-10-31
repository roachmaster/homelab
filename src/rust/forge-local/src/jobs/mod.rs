//! Bounded, volatile, manually stepped local jobs; not OS processes or a scheduler.
//!
//! LocalPrincipal is trusted in-process selection, NOT authenticated identity.
//! Active intent keys do not expire; terminal result and intent expire together
//! on later calls. Once gone, lookup is NotVisible and key reuse may create work.
//! No unexpired record is evicted to admit more work. No durable acceptance,
//! restart recovery, automatic retry, remote ingress, background progress or
//! preemption is supplied. Blocking terminal reads pause the explicit owner pump.
//!
//! Program/policy/clock/destructor code is trusted. Panics propagate and poison
//! the handle; this is fail-closed detection, not rollback or safe recovery.
//! Quotas bound retained counter data, not arbitrary replacement-code allocations.
mod contract;
mod engine;
mod program;
pub use contract::*;
pub use engine::JobsHandle;
use forge_bootstrap::PreparedGroup;
use forge_command::{ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, Presence};
use forge_provider::Factory;
pub use program::*;
use std::{cell::Cell, rc::Rc};

/// Configured dependencies, never a runtime service locator. Clones share the
/// checked creation counter; unrelated factories require distinct base identities.
#[derive(Clone)]
pub struct JobsFactory {
    config: JobsConfig,
    policy: Rc<dyn JobsPolicy>,
    program: Rc<dyn JobProgram>,
    clock: Rc<dyn JobsClock>,
    creations: Rc<Cell<u64>>,
}
impl JobsFactory {
    pub fn with_components(
        config: JobsConfig,
        policy: Rc<dyn JobsPolicy>,
        program: Rc<dyn JobProgram>,
        clock: Rc<dyn JobsClock>,
    ) -> Result<Self, JobsConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            policy,
            program,
            clock,
            creations: Rc::new(Cell::new(0)),
        })
    }
}
impl Factory<JobsHandle> for JobsFactory {
    type Error = JobsConfigError;
    fn create(&self) -> Result<JobsHandle, Self::Error> {
        self.config.validate()?;
        let next = self
            .creations
            .get()
            .checked_add(1)
            .ok_or(JobsConfigError("factory creation IDs exhausted"))?;
        self.creations.set(next);
        let mut config = self.config.clone();
        config.incarnation = format!("{}-{next}", config.incarnation);
        Ok(JobsHandle::new(
            config,
            self.policy.clone(),
            self.program.clone(),
            self.clock.clone(),
        ))
    }
}
pub struct JobsFactories {
    pub service: JobsFactory,
}
/// Validate and stage recipes only. No engine or policy/program/clock callback.
pub fn register(config: JobsConfig) -> Result<PreparedGroup<JobsFactories>, JobsConfigError> {
    register_with(
        config,
        Rc::new(AllowLocal),
        Rc::new(CounterProgram),
        Rc::new(MonotonicClock),
    )
}
/// Explicit local policy/program/clock substitution; same bounded counter contract.
pub fn register_with(
    config: JobsConfig,
    policy: Rc<dyn JobsPolicy>,
    program: Rc<dyn JobProgram>,
    clock: Rc<dyn JobsClock>,
) -> Result<PreparedGroup<JobsFactories>, JobsConfigError> {
    let service = JobsFactory::with_components(config, policy, program, clock)?;
    Ok(PreparedGroup::new(
        "local.jobs",
        &["jobs.service"],
        &[],
        JobsFactories { service },
    )
    .expect("fixed jobs group identifiers are valid"))
}
impl DescribeCommand for JobsHandle {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &DESCRIPTORS
    }
}
const ID: ArgumentDescriptor = ArgumentDescriptor {
    name: "id",
    help: "Local incarnation:sequence job ID returned by submit",
    data_type: DataType::Custom("job-id"),
    presence: Presence::Required,
};
static DESCRIPTORS: [CommandDescriptor; 5] = [
    CommandDescriptor {
        name: "jobs.submit",
        help: "Accept a bounded volatile pure counter; no work until explicit step",
        arguments: &[
            ArgumentDescriptor {
                name: "key",
                help: "Owner-scoped intent key; ASCII identifier, at most 64 bytes",
                data_type: DataType::Text,
                presence: Presence::Required,
            },
            ArgumentDescriptor {
                name: "total",
                help: "Positive counter target within configured work bounds",
                data_type: DataType::U64,
                presence: Presence::Required,
            },
        ],
    },
    CommandDescriptor {
        name: "jobs.inspect",
        help: "Inspect this owner's retained job without executing work",
        arguments: &[ID],
    },
    CommandDescriptor {
        name: "jobs.list",
        help: "List this owner's bounded retained jobs",
        arguments: &[],
    },
    CommandDescriptor {
        name: "jobs.step",
        help: "Execute at most one cooperative quantum; terminal jobs never restart",
        arguments: &[ID],
    },
    CommandDescriptor {
        name: "jobs.cancel",
        help: "Cancel queued work or request cooperative stop; not force-kill",
        arguments: &[ID],
    },
];
#[cfg(test)]
mod tests;
