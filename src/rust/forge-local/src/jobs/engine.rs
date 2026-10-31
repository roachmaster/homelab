use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    time::Instant,
};

struct Record {
    owner: LocalPrincipal,
    key: IntentKey,
    snapshot: JobSnapshot,
    accepted: Instant,
    started: Option<Instant>,
    terminal: Option<Instant>,
    stepper: Option<Box<dyn JobStepper>>,
}
struct Engine {
    records: BTreeMap<u64, Record>,
    next: u64,
    last_now: Instant,
}
struct Inner {
    config: JobsConfig,
    policy: Rc<dyn JobsPolicy>,
    program: Rc<dyn JobProgram>,
    clock: Rc<dyn JobsClock>,
    engine: RefCell<Engine>,
    busy: Cell<bool>,
    poisoned: Cell<bool>,
}
/// Cloning shares one owner-thread engine; this handle is intentionally not Send/Sync.
#[derive(Clone)]
pub struct JobsHandle {
    inner: Rc<Inner>,
}
impl std::fmt::Debug for JobsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobsHandle")
            .field("incarnation", &self.inner.config.incarnation)
            .finish_non_exhaustive()
    }
}
struct Operation<'a> {
    inner: &'a Inner,
    finished: bool,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.inner.poisoned.set(true);
        }
        self.inner.busy.set(false);
    }
}
impl JobsHandle {
    pub(super) fn new(
        config: JobsConfig,
        policy: Rc<dyn JobsPolicy>,
        program: Rc<dyn JobProgram>,
        clock: Rc<dyn JobsClock>,
    ) -> Self {
        let now = clock.now();
        Self {
            inner: Rc::new(Inner {
                config,
                policy,
                program,
                clock,
                engine: RefCell::new(Engine {
                    records: BTreeMap::new(),
                    next: 0,
                    last_now: now,
                }),
                busy: Cell::new(false),
                poisoned: Cell::new(false),
            }),
        }
    }
    /// Capability service for trusted local embedding. Application entrypoints
    /// must still use their shared checked bridge; this is not a network ingress.
    pub fn execute(
        &self,
        owner: &LocalPrincipal,
        command: JobCommand,
    ) -> Result<JobOutput, JobError> {
        if self.inner.poisoned.get() {
            return Err(JobError::Unavailable);
        }
        if self.inner.busy.replace(true) {
            return Err(JobError::Busy);
        }
        let mut operation = Operation {
            inner: &self.inner,
            finished: false,
        };
        let result = self.execute_inner(owner, command);
        operation.finished = true;
        result
    }
    fn execute_inner(
        &self,
        owner: &LocalPrincipal,
        command: JobCommand,
    ) -> Result<JobOutput, JobError> {
        let action = match command {
            JobCommand::Submit { .. } => JobAction::Submit,
            JobCommand::Inspect { .. } => JobAction::Inspect,
            JobCommand::List => JobAction::List,
            JobCommand::Step { .. } => JobAction::Step,
            JobCommand::Cancel { .. } => JobAction::Cancel,
        };
        self.authorize(owner, action)?;
        let now = self.checkpoint()?;
        self.maintain(now)?;
        match command {
            JobCommand::Submit { key, total } => self
                .submit(owner, key, total, now)
                .map(JobOutput::Submitted),
            JobCommand::Inspect { id } => self.snapshot(owner, &id).map(JobOutput::Snapshot),
            JobCommand::List => {
                let engine = self.inner.engine.try_borrow().map_err(|_| JobError::Busy)?;
                Ok(JobOutput::Listed(
                    engine
                        .records
                        .values()
                        .filter(|r| &r.owner == owner)
                        .map(|r| r.snapshot.clone())
                        .collect(),
                ))
            }
            JobCommand::Step { id } => self.step(owner, &id).map(JobOutput::Snapshot),
            JobCommand::Cancel { id } => self.cancel(owner, &id, now).map(JobOutput::Cancelled),
        }
    }
    fn authorize(&self, owner: &LocalPrincipal, action: JobAction) -> Result<(), JobError> {
        self.inner
            .policy
            .authorize(owner, action)
            .map_err(|e| match e {
                PolicyFailure::Denied => JobError::Denied,
                PolicyFailure::Unavailable => JobError::PolicyUnavailable,
            })
    }
    /// Clock callback is always outside a borrow, including post-callback samples.
    fn checkpoint(&self) -> Result<Instant, JobError> {
        let now = self.inner.clock.now();
        let mut engine = self
            .inner
            .engine
            .try_borrow_mut()
            .map_err(|_| JobError::Busy)?;
        if now < engine.last_now {
            self.inner.poisoned.set(true);
            return Err(JobError::Unavailable);
        }
        engine.last_now = now;
        Ok(now)
    }
    fn maintain(&self, now: Instant) -> Result<(), JobError> {
        let mut dropped_steps = Vec::new();
        let mut dropped_records = Vec::new();
        {
            let mut engine = self
                .inner
                .engine
                .try_borrow_mut()
                .map_err(|_| JobError::Busy)?;
            let expired: Vec<_> = engine
                .records
                .iter()
                .filter_map(|(id, record)| {
                    record
                        .terminal
                        .filter(|at| {
                            now.duration_since(*at) >= self.inner.config.terminal_retention
                        })
                        .map(|_| *id)
                })
                .collect();
            for id in expired {
                if let Some(record) = engine.records.remove(&id) {
                    dropped_records.push(record);
                }
            }
            for record in engine.records.values_mut() {
                if record.snapshot.state == JobState::Queued
                    && now.duration_since(record.accepted) >= self.inner.config.queue_ttl
                {
                    self.advance(record)?;
                    record.snapshot.state = JobState::Expired;
                    record.terminal = Some(now);
                    if let Some(stepper) = record.stepper.take() {
                        dropped_steps.push(stepper);
                    }
                }
            }
        }
        drop(dropped_steps);
        drop(dropped_records); // No custom destruction while engine is borrowed.
        Ok(())
    }
    fn advance(&self, record: &mut Record) -> Result<(), JobError> {
        record.snapshot.revision = record.snapshot.revision.checked_add(1).ok_or_else(|| {
            self.inner.poisoned.set(true);
            JobError::IdsExhausted
        })?;
        Ok(())
    }
    fn check_id(&self, id: &JobId) -> Result<(), JobError> {
        if id.incarnation != self.inner.config.incarnation {
            return Err(JobError::WrongIncarnation);
        }
        Ok(())
    }
    fn owned<'a>(
        engine: &'a Engine,
        owner: &LocalPrincipal,
        id: &JobId,
    ) -> Result<&'a Record, JobError> {
        engine
            .records
            .get(&id.sequence)
            .filter(|r| &r.owner == owner)
            .ok_or(JobError::NotVisible)
    }
    fn owned_mut<'a>(
        engine: &'a mut Engine,
        owner: &LocalPrincipal,
        id: &JobId,
    ) -> Result<&'a mut Record, JobError> {
        engine
            .records
            .get_mut(&id.sequence)
            .filter(|r| &r.owner == owner)
            .ok_or(JobError::NotVisible)
    }
    fn receipt(&self, id: JobId) -> Receipt {
        Receipt {
            id,
            revision: 1,
            terminal_retention: self.inner.config.terminal_retention,
        }
    }
    fn snapshot(&self, owner: &LocalPrincipal, id: &JobId) -> Result<JobSnapshot, JobError> {
        self.check_id(id)?;
        let engine = self.inner.engine.try_borrow().map_err(|_| JobError::Busy)?;
        Ok(Self::owned(&engine, owner, id)?.snapshot.clone())
    }
    fn submit(
        &self,
        owner: &LocalPrincipal,
        key: IntentKey,
        total: u64,
        now: Instant,
    ) -> Result<Receipt, JobError> {
        if total == 0 || total > self.inner.config.max_total {
            return Err(JobError::Invalid("total"));
        }
        let id;
        {
            let mut engine = self
                .inner
                .engine
                .try_borrow_mut()
                .map_err(|_| JobError::Busy)?;
            if let Some(old) = engine
                .records
                .values()
                .find(|r| &r.owner == owner && r.key == key)
            {
                return if old.snapshot.total == total {
                    Ok(self.receipt(old.snapshot.id.clone()))
                } else {
                    Err(JobError::IntentConflict)
                };
            }
            if engine.records.len() >= self.inner.config.max_records {
                return Err(JobError::Capacity(Capacity::Records));
            }
            if engine
                .records
                .values()
                .filter(|r| r.snapshot.state == JobState::Queued)
                .count()
                >= self.inner.config.max_queued
            {
                return Err(JobError::Capacity(Capacity::Queued));
            }
            if engine
                .records
                .values()
                .filter(|r| &r.owner == owner && !r.snapshot.state.is_terminal())
                .count()
                >= self.inner.config.max_per_owner
            {
                return Err(JobError::Capacity(Capacity::PerOwner));
            }
            engine.next = engine.next.checked_add(1).ok_or(JobError::IdsExhausted)?;
            id = JobId {
                incarnation: self.inner.config.incarnation.clone(),
                sequence: engine.next,
            };
            // Record, intent and queue reservation are one private transaction.
            engine.records.insert(
                id.sequence,
                Record {
                    owner: owner.clone(),
                    key,
                    snapshot: JobSnapshot {
                        id: id.clone(),
                        revision: 1,
                        state: JobState::Queued,
                        completed: 0,
                        total,
                        cancellation: None,
                        result: None,
                        failure: None,
                    },
                    accepted: now,
                    started: None,
                    terminal: None,
                    stepper: None,
                },
            );
        }
        let prepared = self.inner.program.prepare(total);
        let committed = self.checkpoint()?;
        let mut removed = None;
        let result;
        {
            let mut engine = self
                .inner
                .engine
                .try_borrow_mut()
                .map_err(|_| JobError::Busy)?;
            result = match prepared {
                Ok(stepper) => {
                    let Some(record) = engine.records.get_mut(&id.sequence) else {
                        self.inner.poisoned.set(true);
                        drop(engine);
                        drop(stepper);
                        return Err(JobError::Unavailable);
                    };
                    record.accepted = committed;
                    record.stepper = Some(stepper);
                    Ok(self.receipt(id))
                }
                Err(_) => {
                    removed = engine.records.remove(&id.sequence);
                    Err(JobError::PreparationFailed)
                }
            };
        }
        drop(removed);
        result
    }
    fn cancel(
        &self,
        owner: &LocalPrincipal,
        id: &JobId,
        now: Instant,
    ) -> Result<CancelAck, JobError> {
        self.check_id(id)?;
        let mut dropped = None;
        let response;
        {
            let mut engine = self
                .inner
                .engine
                .try_borrow_mut()
                .map_err(|_| JobError::Busy)?;
            let record = Self::owned_mut(&mut engine, owner, id)?;
            response = match record.snapshot.state {
                JobState::Queued => {
                    self.advance(record)?;
                    record.snapshot.state = JobState::Cancelled;
                    record.snapshot.cancellation = Some(CancelReason::User);
                    record.terminal = Some(now);
                    dropped = record.stepper.take();
                    CancelAck::CancelledBeforeStart(record.snapshot.clone())
                }
                JobState::Running => {
                    if record.snapshot.cancellation.is_none() {
                        self.advance(record)?;
                        record.snapshot.cancellation = Some(CancelReason::User);
                    }
                    CancelAck::Requested(record.snapshot.clone())
                }
                _ => CancelAck::AlreadyTerminal(record.snapshot.clone()),
            };
        }
        drop(dropped);
        Ok(response)
    }
    fn step(&self, owner: &LocalPrincipal, id: &JobId) -> Result<JobSnapshot, JobError> {
        self.check_id(id)?;
        {
            let engine = self.inner.engine.try_borrow().map_err(|_| JobError::Busy)?;
            let record = Self::owned(&engine, owner, id)?;
            if record.snapshot.state.is_terminal() {
                return Ok(record.snapshot.clone());
            }
            if record.snapshot.state == JobState::Queued
                && engine
                    .records
                    .values()
                    .filter(|r| r.snapshot.state == JobState::Running)
                    .count()
                    >= self.inner.config.max_active
            {
                return Err(JobError::Capacity(Capacity::Active));
            }
        }
        let permission = self.inner.policy.authorize(owner, JobAction::Execute);
        let now = self.checkpoint()?; // Never use a timestamp from before policy code.
        let (mut stepper, expected_revision, context);
        {
            let mut engine = self
                .inner
                .engine
                .try_borrow_mut()
                .map_err(|_| JobError::Busy)?;
            let record = Self::owned_mut(&mut engine, owner, id)?;
            let stopped = if record.snapshot.state == JobState::Queued
                && now.duration_since(record.accepted) >= self.inner.config.queue_ttl
            {
                Some((JobState::Expired, None))
            } else if let Err(reason) = permission {
                Some((
                    JobState::Failed,
                    Some(match reason {
                        PolicyFailure::Denied => FailureReason::PermissionDenied,
                        PolicyFailure::Unavailable => FailureReason::PolicyUnavailable,
                    }),
                ))
            } else {
                None
            };
            if let Some((state, failure)) = stopped {
                self.advance(record)?;
                record.snapshot.state = state;
                record.snapshot.failure = failure;
                record.terminal = Some(now);
                let dropped = record.stepper.take();
                let snapshot = record.snapshot.clone();
                drop(engine);
                drop(dropped);
                return Ok(snapshot);
            }
            if record.snapshot.state == JobState::Queued {
                self.advance(record)?;
                record.snapshot.state = JobState::Running;
                record.started = Some(now);
            }
            if record.snapshot.cancellation.is_none()
                && record
                    .started
                    .is_some_and(|at| now.duration_since(at) >= self.inner.config.runtime_budget)
            {
                self.advance(record)?;
                record.snapshot.cancellation = Some(CancelReason::RuntimeBudget);
            }
            stepper = record.stepper.take().ok_or_else(|| {
                self.inner.poisoned.set(true);
                JobError::Unavailable
            })?;
            expected_revision = record.snapshot.revision;
            context = StepContext {
                max_work_units: self.inner.config.quantum,
                cancellation: record.snapshot.cancellation,
            };
        }
        // The record stays Running, so taking its token never frees active capacity.
        let outcome = stepper.step(context);
        let finished = self.checkpoint()?; // Retention begins at completion, not call entry.
        let mut returned = Some(stepper);
        let snapshot;
        {
            let mut engine = self
                .inner
                .engine
                .try_borrow_mut()
                .map_err(|_| JobError::Busy)?;
            let record = Self::owned_mut(&mut engine, owner, id)?;
            if record.snapshot.revision != expected_revision
                || record.snapshot.state != JobState::Running
            {
                self.inner.poisoned.set(true);
                return Err(JobError::Unavailable);
            }
            self.advance(record)?;
            let valid = |value: u64| {
                value >= record.snapshot.completed
                    && value <= record.snapshot.total
                    && value - record.snapshot.completed <= u64::from(context.max_work_units)
            };
            match outcome {
                StepResult::Yielded { completed }
                    if valid(completed) && completed < record.snapshot.total =>
                {
                    record.snapshot.completed = completed;
                    record.stepper = returned.take();
                }
                StepResult::Succeeded { value }
                    if valid(value) && value == record.snapshot.total =>
                {
                    record.snapshot.completed = value;
                    record.snapshot.result = Some(value);
                    record.snapshot.state = JobState::Succeeded;
                }
                StepResult::Failed => {
                    record.snapshot.state = JobState::Failed;
                    record.snapshot.failure = Some(FailureReason::ProgramFailed);
                }
                StepResult::Cancelled if context.cancellation.is_some() => {
                    record.snapshot.state = JobState::Cancelled
                }
                _ => {
                    record.snapshot.state = JobState::Failed;
                    record.snapshot.failure = Some(FailureReason::InvalidProgress);
                }
            }
            if record.snapshot.state.is_terminal() {
                record.terminal = Some(finished);
            }
            snapshot = record.snapshot.clone();
        }
        drop(returned);
        Ok(snapshot)
    }
}

#[cfg(test)]
mod invariant_tests {
    use super::*;
    fn handle() -> JobsHandle {
        JobsHandle::new(
            JobsConfig::local("test"),
            Rc::new(AllowLocal),
            Rc::new(CounterProgram),
            Rc::new(MonotonicClock),
        )
    }
    fn submit(handle: &JobsHandle) -> JobId {
        let JobOutput::Submitted(receipt) = handle
            .execute(
                &LocalPrincipal::new("a").unwrap(),
                JobCommand::Submit {
                    key: IntentKey::new("key").unwrap(),
                    total: 2,
                },
            )
            .unwrap()
        else {
            panic!("receipt")
        };
        receipt.id
    }
    #[test]
    fn sequence_exhaustion_never_wraps_or_admits() {
        let h = handle();
        h.inner.engine.borrow_mut().next = u64::MAX;
        assert_eq!(
            h.execute(
                &LocalPrincipal::new("a").unwrap(),
                JobCommand::Submit {
                    key: IntentKey::new("key").unwrap(),
                    total: 2
                }
            ),
            Err(JobError::IdsExhausted)
        );
        assert!(h.inner.engine.borrow().records.is_empty());
    }
    #[test]
    fn revision_exhaustion_poisoning_is_not_success() {
        let h = handle();
        let id = submit(&h);
        h.inner
            .engine
            .borrow_mut()
            .records
            .get_mut(&id.sequence)
            .unwrap()
            .snapshot
            .revision = u64::MAX;
        assert_eq!(
            h.execute(
                &LocalPrincipal::new("a").unwrap(),
                JobCommand::Cancel { id }
            ),
            Err(JobError::IdsExhausted)
        );
        assert_eq!(
            h.execute(&LocalPrincipal::new("a").unwrap(), JobCommand::List),
            Err(JobError::Unavailable)
        );
    }
    #[derive(Clone)]
    struct Probe {
        slot: Rc<RefCell<Option<JobsHandle>>>,
        calls: Rc<Cell<usize>>,
        now: Instant,
    }
    impl Probe {
        fn check(&self) {
            if let Some(handle) = self.slot.borrow().as_ref() {
                assert!(
                    handle.inner.engine.try_borrow_mut().is_ok(),
                    "callback saw a borrowed engine"
                );
                self.calls.set(self.calls.get() + 1);
            }
        }
    }
    impl JobsPolicy for Probe {
        fn authorize(&self, _: &LocalPrincipal, _: JobAction) -> Result<(), PolicyFailure> {
            self.check();
            Ok(())
        }
    }
    impl JobsClock for Probe {
        fn now(&self) -> Instant {
            self.check();
            self.now
        }
    }
    impl JobProgram for Probe {
        fn prepare(&self, _: u64) -> Result<Box<dyn JobStepper>, PrepareError> {
            self.check();
            Ok(Box::new(ProbeStep(self.clone())))
        }
    }
    struct ProbeStep(Probe);
    impl JobStepper for ProbeStep {
        fn step(&mut self, _: StepContext) -> StepResult {
            self.0.check();
            StepResult::Succeeded { value: 2 }
        }
    }
    impl Drop for ProbeStep {
        fn drop(&mut self) {
            self.0.check();
        }
    }
    #[test]
    fn policy_clock_preparation_step_and_custom_drop_run_without_engine_borrows() {
        let slot = Rc::new(RefCell::new(None));
        let calls = Rc::new(Cell::new(0));
        let probe = Rc::new(Probe {
            slot: slot.clone(),
            calls: calls.clone(),
            now: Instant::now(),
        });
        let h = JobsHandle::new(
            JobsConfig::local("probe"),
            probe.clone(),
            probe.clone(),
            probe,
        );
        *slot.borrow_mut() = Some(h.clone());
        let id = submit(&h);
        h.execute(&LocalPrincipal::new("a").unwrap(), JobCommand::Step { id })
            .unwrap();
        assert!(calls.get() >= 8);
        *slot.borrow_mut() = None;
    }
}
