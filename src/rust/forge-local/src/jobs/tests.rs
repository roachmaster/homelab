use super::*;
use forge_bootstrap::Registry;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    panic::{catch_unwind, AssertUnwindSafe},
    time::{Duration, Instant},
};

struct Clock(Cell<Instant>);
impl Clock {
    fn new() -> Self {
        Self(Cell::new(Instant::now()))
    }
    fn advance(&self, n: u64) {
        self.0.set(self.0.get() + Duration::from_secs(n));
    }
}
impl JobsClock for Clock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
fn who(name: &str) -> LocalPrincipal {
    LocalPrincipal::new(name).unwrap()
}
fn config() -> JobsConfig {
    let mut c = JobsConfig::local("test");
    c.quantum = 2;
    c.max_total = 100;
    c.queue_ttl = Duration::from_secs(10);
    c.runtime_budget = Duration::from_secs(5);
    c.terminal_retention = Duration::from_secs(20);
    c
}
fn factory(
    c: JobsConfig,
    policy: Rc<dyn JobsPolicy>,
    program: Rc<dyn JobProgram>,
    clock: Rc<dyn JobsClock>,
) -> JobsFactory {
    let group = register_with(c, policy, program, clock).unwrap();
    let ((), factories) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factories.service
}
fn service(
    c: JobsConfig,
    policy: Rc<dyn JobsPolicy>,
    program: Rc<dyn JobProgram>,
    clock: Rc<dyn JobsClock>,
) -> JobsHandle {
    factory(c, policy, program, clock).create().unwrap()
}
fn ordinary() -> (JobsHandle, Rc<Clock>) {
    let clock = Rc::new(Clock::new());
    (
        service(
            config(),
            Rc::new(AllowLocal),
            Rc::new(CounterProgram),
            clock.clone(),
        ),
        clock,
    )
}
fn submit(
    h: &JobsHandle,
    owner: &LocalPrincipal,
    key: &str,
    total: u64,
) -> Result<Receipt, JobError> {
    match h.execute(
        owner,
        JobCommand::Submit {
            key: IntentKey::new(key).unwrap(),
            total,
        },
    )? {
        JobOutput::Submitted(receipt) => Ok(receipt),
        _ => panic!("receipt"),
    }
}
fn inspect(h: &JobsHandle, owner: &LocalPrincipal, id: &JobId) -> JobSnapshot {
    let JobOutput::Snapshot(s) = h
        .execute(owner, JobCommand::Inspect { id: id.clone() })
        .unwrap()
    else {
        panic!("snapshot")
    };
    s
}
fn step(h: &JobsHandle, owner: &LocalPrincipal, id: &JobId) -> JobSnapshot {
    let JobOutput::Snapshot(s) = h
        .execute(owner, JobCommand::Step { id: id.clone() })
        .unwrap()
    else {
        panic!("snapshot")
    };
    s
}
fn list(h: &JobsHandle, owner: &LocalPrincipal) -> Vec<JobSnapshot> {
    let JobOutput::Listed(s) = h.execute(owner, JobCommand::List).unwrap() else {
        panic!("list")
    };
    s
}

#[test]
fn counter_only_progresses_on_steps_and_terminal_never_restarts() {
    let (h, _) = ordinary();
    let a = who("a");
    let r = submit(&h, &a, "k", 5).unwrap();
    assert_eq!(inspect(&h, &a, &r.id).state, JobState::Queued);
    assert_eq!(list(&h, &a)[0].completed, 0);
    assert_eq!(step(&h, &a, &r.id).completed, 2);
    assert_eq!(step(&h, &a, &r.id).completed, 4);
    let done = step(&h, &a, &r.id);
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.result, Some(5));
    assert_eq!(step(&h, &a, &r.id), done);
    assert_eq!(
        h.execute(&a, JobCommand::Cancel { id: r.id }).unwrap(),
        JobOutput::Cancelled(CancelAck::AlreadyTerminal(done))
    );
}
#[test]
fn cancellation_is_queued_stop_or_running_request_then_confirmation() {
    let (h, _) = ordinary();
    let a = who("a");
    let q = submit(&h, &a, "q", 5).unwrap();
    assert!(matches!(
        h.execute(&a, JobCommand::Cancel { id: q.id.clone() }),
        Ok(JobOutput::Cancelled(CancelAck::CancelledBeforeStart(_)))
    ));
    assert_eq!(step(&h, &a, &q.id).completed, 0);
    let r = submit(&h, &a, "r", 5).unwrap();
    step(&h, &a, &r.id);
    let first = h
        .execute(&a, JobCommand::Cancel { id: r.id.clone() })
        .unwrap();
    assert!(matches!(
        first,
        JobOutput::Cancelled(CancelAck::Requested(_))
    ));
    assert_eq!(
        h.execute(&a, JobCommand::Cancel { id: r.id.clone() })
            .unwrap(),
        first
    );
    assert_eq!(inspect(&h, &a, &r.id).state, JobState::Running);
    let stopped = step(&h, &a, &r.id);
    assert_eq!(stopped.state, JobState::Cancelled);
    assert_eq!(stopped.completed, 2);
}
#[test]
fn intent_replay_returns_original_receipt_after_execution_and_conflict_preserves_it() {
    let (h, _) = ordinary();
    let a = who("a");
    let original = submit(&h, &a, "k", 2).unwrap();
    assert_eq!(submit(&h, &a, "k", 2), Ok(original.clone()));
    step(&h, &a, &original.id);
    assert_eq!(submit(&h, &a, "k", 2), Ok(original.clone()));
    assert_eq!(submit(&h, &a, "k", 3), Err(JobError::IntentConflict));
    assert_eq!(list(&h, &a).len(), 1);
    assert_eq!(original.revision, 1);
}
#[test]
fn ownership_and_incarnation_are_independent_of_policy_allow() {
    let (h, _) = ordinary();
    let a = who("a");
    let b = who("b");
    let first = submit(&h, &a, "shared", 5).unwrap();
    let second = submit(&h, &b, "shared", 5).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(list(&h, &b).len(), 1);
    for c in [
        JobCommand::Inspect {
            id: first.id.clone(),
        },
        JobCommand::Step {
            id: first.id.clone(),
        },
        JobCommand::Cancel {
            id: first.id.clone(),
        },
    ] {
        assert_eq!(h.execute(&b, c), Err(JobError::NotVisible));
    }
    assert_eq!(inspect(&h, &a, &first.id).state, JobState::Queued);
    assert_eq!(
        h.execute(
            &a,
            JobCommand::Inspect {
                id: "other:1".parse().unwrap()
            }
        ),
        Err(JobError::WrongIncarnation)
    );
}
#[test]
fn queue_expiry_and_running_budget_have_distinct_meanings() {
    let (h, clock) = ordinary();
    let a = who("a");
    let q = submit(&h, &a, "q", 5).unwrap();
    clock.advance(10);
    let expired = step(&h, &a, &q.id);
    assert_eq!(expired.state, JobState::Expired);
    assert_eq!(expired.completed, 0);
    let r = submit(&h, &a, "r", 5).unwrap();
    step(&h, &a, &r.id);
    clock.advance(5);
    let cancelled = step(&h, &a, &r.id);
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(cancelled.cancellation, Some(CancelReason::RuntimeBudget));
}
#[test]
fn terminal_intents_retained_without_unexpired_eviction_then_expire_together() {
    let clock = Rc::new(Clock::new());
    let mut c = config();
    c.max_records = 1;
    c.max_queued = 1;
    c.max_active = 1;
    c.max_per_owner = 1;
    let h = service(
        c,
        Rc::new(AllowLocal),
        Rc::new(CounterProgram),
        clock.clone(),
    );
    let a = who("a");
    let old = submit(&h, &a, "old", 1).unwrap();
    step(&h, &a, &old.id);
    assert_eq!(
        submit(&h, &a, "new", 1),
        Err(JobError::Capacity(Capacity::Records))
    );
    clock.advance(19);
    assert_eq!(submit(&h, &a, "old", 1), Ok(old.clone()));
    clock.advance(1);
    assert_eq!(
        h.execute(&a, JobCommand::Inspect { id: old.id.clone() }),
        Err(JobError::NotVisible)
    );
    assert_ne!(submit(&h, &a, "old", 1).unwrap().id, old.id);
}
#[test]
fn active_intent_survives_terminal_retention_window() {
    let clock = Rc::new(Clock::new());
    let mut c = config();
    c.queue_ttl = Duration::from_secs(100);
    c.runtime_budget = Duration::from_secs(100);
    let h = service(
        c,
        Rc::new(AllowLocal),
        Rc::new(CounterProgram),
        clock.clone(),
    );
    let a = who("a");
    let r = submit(&h, &a, "k", 5).unwrap();
    step(&h, &a, &r.id);
    clock.advance(30);
    assert_eq!(submit(&h, &a, "k", 5), Ok(r));
}
#[test]
fn queue_active_and_owner_limits_are_separate() {
    let clock = Rc::new(Clock::new());
    let mut c = config();
    c.max_queued = 1;
    c.max_active = 1;
    c.max_per_owner = 3;
    let h = service(c, Rc::new(AllowLocal), Rc::new(CounterProgram), clock);
    let a = who("a");
    let r = submit(&h, &a, "one", 5).unwrap();
    assert_eq!(
        submit(&h, &a, "two", 5),
        Err(JobError::Capacity(Capacity::Queued))
    );
    step(&h, &a, &r.id);
    let second = submit(&h, &a, "two", 5).unwrap();
    assert_eq!(
        h.execute(&a, JobCommand::Step { id: second.id }),
        Err(JobError::Capacity(Capacity::Active))
    );
    let mut c = config();
    c.max_per_owner = 1;
    let h = service(
        c,
        Rc::new(AllowLocal),
        Rc::new(CounterProgram),
        Rc::new(Clock::new()),
    );
    submit(&h, &a, "one", 5).unwrap();
    assert_eq!(
        submit(&h, &a, "two", 5),
        Err(JobError::Capacity(Capacity::PerOwner))
    );
    assert!(submit(&h, &who("b"), "two", 5).is_ok());
}
#[test]
fn identifiers_and_configuration_are_bounded() {
    for s in ["", "bad space", "bad:colon", "ümlaut"] {
        assert!(LocalPrincipal::new(s).is_err());
        assert!(IntentKey::new(s).is_err());
    }
    assert!(IntentKey::new("x".repeat(65)).is_err());
    assert!("x:0".parse::<JobId>().is_err());
    assert!(format!("x:{}", "9".repeat(1000)).parse::<JobId>().is_err());
    assert!(JobsConfig::local("x".repeat(43)).validate().is_err());
    let mut c = config();
    c.quantum = 4097;
    assert!(c.validate().is_err());
    c = config();
    c.max_records = 0;
    assert!(c.validate().is_err());
    c = config();
    c.terminal_retention = Duration::ZERO;
    assert!(c.validate().is_err());
    let (h, _) = ordinary();
    assert_eq!(
        submit(&h, &who("a"), "zero", 0),
        Err(JobError::Invalid("total"))
    );
    assert_eq!(
        submit(&h, &who("a"), "big", 101),
        Err(JobError::Invalid("total"))
    );
    assert!(list(&h, &who("a")).is_empty());
}

struct ScriptProgram {
    outcomes: Vec<StepResult>,
    prepares: Rc<Cell<usize>>,
    steps: Rc<Cell<usize>>,
    drops: Rc<Cell<usize>>,
    prepare_hook: Option<Rc<dyn Fn()>>,
    step_hook: Option<Rc<dyn Fn()>>,
    fail_prepare: bool,
}
impl JobProgram for ScriptProgram {
    fn prepare(&self, _: u64) -> Result<Box<dyn JobStepper>, PrepareError> {
        self.prepares.set(self.prepares.get() + 1);
        if let Some(hook) = &self.prepare_hook {
            hook();
        }
        if self.fail_prepare {
            return Err(PrepareError);
        }
        Ok(Box::new(ScriptStepper {
            outcomes: self.outcomes.clone().into(),
            steps: self.steps.clone(),
            drops: self.drops.clone(),
            hook: self.step_hook.clone(),
        }))
    }
}
struct ScriptStepper {
    outcomes: VecDeque<StepResult>,
    steps: Rc<Cell<usize>>,
    drops: Rc<Cell<usize>>,
    hook: Option<Rc<dyn Fn()>>,
}
impl JobStepper for ScriptStepper {
    fn step(&mut self, _: StepContext) -> StepResult {
        self.steps.set(self.steps.get() + 1);
        if let Some(hook) = &self.hook {
            hook();
        }
        self.outcomes.pop_front().unwrap_or(StepResult::Failed)
    }
}
impl Drop for ScriptStepper {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}
fn script(outcomes: Vec<StepResult>) -> ScriptProgram {
    ScriptProgram {
        outcomes,
        prepares: Rc::new(Cell::new(0)),
        steps: Rc::new(Cell::new(0)),
        drops: Rc::new(Cell::new(0)),
        prepare_hook: None,
        step_hook: None,
        fail_prepare: false,
    }
}
struct Policy {
    action: JobAction,
    failure: PolicyFailure,
}
impl JobsPolicy for Policy {
    fn authorize(&self, _: &LocalPrincipal, action: JobAction) -> Result<(), PolicyFailure> {
        if action == self.action {
            Err(self.failure)
        } else {
            Ok(())
        }
    }
}
#[test]
fn denied_admission_never_prepares() {
    let p = script(vec![]);
    let count = p.prepares.clone();
    let h = service(
        config(),
        Rc::new(Policy {
            action: JobAction::Submit,
            failure: PolicyFailure::Denied,
        }),
        Rc::new(p),
        Rc::new(Clock::new()),
    );
    assert_eq!(submit(&h, &who("a"), "k", 1), Err(JobError::Denied));
    assert_eq!(count.get(), 0);
}
#[test]
fn execution_recheck_denial_and_outage_fail_without_step_or_retry() {
    for (failure, expected) in [
        (PolicyFailure::Denied, FailureReason::PermissionDenied),
        (PolicyFailure::Unavailable, FailureReason::PolicyUnavailable),
    ] {
        let p = script(vec![StepResult::Succeeded { value: 1 }]);
        let count = p.steps.clone();
        let h = service(
            config(),
            Rc::new(Policy {
                action: JobAction::Execute,
                failure,
            }),
            Rc::new(p),
            Rc::new(Clock::new()),
        );
        let r = submit(&h, &who("a"), "k", 1).unwrap();
        let s = step(&h, &who("a"), &r.id);
        assert_eq!(s.failure, Some(expected));
        assert_eq!(s.state, JobState::Failed);
        assert_eq!(step(&h, &who("a"), &r.id), s);
        assert_eq!(count.get(), 0);
    }
}
#[test]
fn preparation_failure_releases_all_reservations_without_receipt() {
    let mut p = script(vec![]);
    p.fail_prepare = true;
    let count = p.prepares.clone();
    let mut c = config();
    c.max_queued = 1;
    c.max_records = 1;
    c.max_active = 1;
    c.max_per_owner = 1;
    let h = service(c, Rc::new(AllowLocal), Rc::new(p), Rc::new(Clock::new()));
    for _ in 0..2 {
        assert_eq!(
            submit(&h, &who("a"), "k", 1),
            Err(JobError::PreparationFailed)
        );
        assert!(list(&h, &who("a")).is_empty());
    }
    assert_eq!(count.get(), 2);
}
#[test]
fn program_failure_and_invalid_progress_are_terminal() {
    for (outcome, reason) in [
        (StepResult::Failed, FailureReason::ProgramFailed),
        (
            StepResult::Yielded { completed: 3 },
            FailureReason::InvalidProgress,
        ),
        (
            StepResult::Succeeded { value: 1 },
            FailureReason::InvalidProgress,
        ),
        (StepResult::Cancelled, FailureReason::InvalidProgress),
    ] {
        let h = service(
            config(),
            Rc::new(AllowLocal),
            Rc::new(script(vec![outcome])),
            Rc::new(Clock::new()),
        );
        let r = submit(&h, &who("a"), "k", 5).unwrap();
        let s = step(&h, &who("a"), &r.id);
        assert_eq!(s.state, JobState::Failed);
        assert_eq!(s.failure, Some(reason));
        assert_eq!(step(&h, &who("a"), &r.id), s);
    }
}
#[test]
fn completion_can_win_after_cancel_request() {
    let mut c = config();
    c.quantum = 1;
    let h = service(
        c,
        Rc::new(AllowLocal),
        Rc::new(script(vec![
            StepResult::Yielded { completed: 1 },
            StepResult::Succeeded { value: 2 },
        ])),
        Rc::new(Clock::new()),
    );
    let r = submit(&h, &who("a"), "k", 2).unwrap();
    step(&h, &who("a"), &r.id);
    h.execute(&who("a"), JobCommand::Cancel { id: r.id.clone() })
        .unwrap();
    assert_eq!(step(&h, &who("a"), &r.id).state, JobState::Succeeded);
}
#[test]
fn preparation_and_step_reentrancy_return_busy_not_partial_receipts() {
    for during_prepare in [true, false] {
        let slot = Rc::new(RefCell::new(None::<JobsHandle>));
        let other = slot.clone();
        let observed = Rc::new(Cell::new(0));
        let count = observed.clone();
        let hook: Rc<dyn Fn()> = Rc::new(move || {
            let h = other.borrow();
            assert_eq!(
                h.as_ref().unwrap().execute(&who("a"), JobCommand::List),
                Err(JobError::Busy)
            );
            count.set(count.get() + 1);
        });
        let mut p = script(vec![StepResult::Succeeded { value: 1 }]);
        if during_prepare {
            p.prepare_hook = Some(hook);
        } else {
            p.step_hook = Some(hook);
        }
        let h = service(
            config(),
            Rc::new(AllowLocal),
            Rc::new(p),
            Rc::new(Clock::new()),
        );
        *slot.borrow_mut() = Some(h.clone());
        let r = submit(&h, &who("a"), "k", 1).unwrap();
        step(&h, &who("a"), &r.id);
        assert_eq!(observed.get(), 1);
        *slot.borrow_mut() = None;
    }
}
#[test]
fn callback_panics_propagate_and_poison_instead_of_resuming_partial_state() {
    for prepare in [true, false] {
        let mut p = script(vec![StepResult::Succeeded { value: 1 }]);
        let hook: Rc<dyn Fn()> = Rc::new(|| panic!("trusted callback panic"));
        if prepare {
            p.prepare_hook = Some(hook);
        } else {
            p.step_hook = Some(hook);
        }
        let h = service(
            config(),
            Rc::new(AllowLocal),
            Rc::new(p),
            Rc::new(Clock::new()),
        );
        let caught = catch_unwind(AssertUnwindSafe(|| {
            let r = submit(&h, &who("a"), "k", 1).unwrap();
            step(&h, &who("a"), &r.id);
        }));
        assert!(caught.is_err());
        assert_eq!(
            h.execute(&who("a"), JobCommand::List),
            Err(JobError::Unavailable)
        );
    }
}
#[test]
fn terminalization_and_host_drop_release_program_state_once() {
    let p = script(vec![StepResult::Succeeded { value: 1 }]);
    let drops = p.drops.clone();
    let h = service(
        config(),
        Rc::new(AllowLocal),
        Rc::new(p),
        Rc::new(Clock::new()),
    );
    let r = submit(&h, &who("a"), "one", 1).unwrap();
    step(&h, &who("a"), &r.id);
    assert_eq!(drops.get(), 1);
    let r = submit(&h, &who("a"), "two", 1).unwrap();
    h.execute(&who("a"), JobCommand::Cancel { id: r.id })
        .unwrap();
    assert_eq!(drops.get(), 2);
    submit(&h, &who("a"), "three", 1).unwrap();
    drop(h);
    assert_eq!(drops.get(), 3);
}
struct AdvancingPolicy {
    clock: Rc<Clock>,
}
impl JobsPolicy for AdvancingPolicy {
    fn authorize(&self, _: &LocalPrincipal, action: JobAction) -> Result<(), PolicyFailure> {
        if action == JobAction::Execute {
            self.clock.advance(11);
        }
        Ok(())
    }
}
#[test]
fn queue_ttl_rechecks_after_slow_policy_before_any_step() {
    let clock = Rc::new(Clock::new());
    let p = script(vec![StepResult::Succeeded { value: 1 }]);
    let calls = p.steps.clone();
    let h = service(
        config(),
        Rc::new(AdvancingPolicy {
            clock: clock.clone(),
        }),
        Rc::new(p),
        clock,
    );
    let r = submit(&h, &who("a"), "k", 1).unwrap();
    assert_eq!(step(&h, &who("a"), &r.id).state, JobState::Expired);
    assert_eq!(calls.get(), 0);
}
#[test]
fn acceptance_and_terminal_retention_start_at_callback_commit_times() {
    let clock = Rc::new(Clock::new());
    let mut p = script(vec![StepResult::Succeeded { value: 1 }]);
    let c = clock.clone();
    p.prepare_hook = Some(Rc::new(move || c.advance(11)));
    let c = clock.clone();
    p.step_hook = Some(Rc::new(move || c.advance(30)));
    let h = service(config(), Rc::new(AllowLocal), Rc::new(p), clock.clone());
    let r = submit(&h, &who("a"), "k", 1).unwrap();
    assert_eq!(inspect(&h, &who("a"), &r.id).state, JobState::Queued);
    assert_eq!(step(&h, &who("a"), &r.id).state, JobState::Succeeded);
    clock.advance(19);
    assert_eq!(inspect(&h, &who("a"), &r.id).state, JobState::Succeeded);
    clock.advance(1);
    assert_eq!(
        h.execute(&who("a"), JobCommand::Inspect { id: r.id }),
        Err(JobError::NotVisible)
    );
}
#[test]
fn backwards_clock_fails_closed_not_budget_reset() {
    let (h, clock) = ordinary();
    submit(&h, &who("a"), "k", 1).unwrap();
    clock.0.set(clock.0.get() - Duration::from_secs(1));
    assert_eq!(
        h.execute(&who("a"), JobCommand::List),
        Err(JobError::Unavailable)
    );
    clock.advance(2);
    assert_eq!(
        h.execute(&who("a"), JobCommand::List),
        Err(JobError::Unavailable)
    );
}
struct CountClock {
    calls: Rc<Cell<usize>>,
    time: Instant,
}
impl JobsClock for CountClock {
    fn now(&self) -> Instant {
        self.calls.set(self.calls.get() + 1);
        self.time
    }
}
#[test]
fn registration_is_effect_free_and_factory_clones_create_isolated_incarnations() {
    let calls = Rc::new(Cell::new(0));
    let clock = Rc::new(CountClock {
        calls: calls.clone(),
        time: Instant::now(),
    });
    let f = factory(
        config(),
        Rc::new(AllowLocal),
        Rc::new(CounterProgram),
        clock,
    );
    assert_eq!(calls.get(), 0);
    let first = f.create().unwrap();
    let second = f.clone().create().unwrap();
    assert_eq!(calls.get(), 2);
    let a = submit(&first, &who("a"), "k", 1).unwrap();
    let b = submit(&second, &who("a"), "k", 1).unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(
        second.execute(&who("a"), JobCommand::Inspect { id: a.id }),
        Err(JobError::WrongIncarnation)
    );
}
#[test]
fn exhausted_factory_counter_is_fallible_without_clock_calls_or_id_reuse() {
    let calls = Rc::new(Cell::new(0));
    let f = factory(
        config(),
        Rc::new(AllowLocal),
        Rc::new(CounterProgram),
        Rc::new(CountClock {
            calls: calls.clone(),
            time: Instant::now(),
        }),
    );
    f.creations.set(u64::MAX);
    assert!(f.clone().create().is_err());
    assert_eq!(calls.get(), 0);
}

#[test]
fn runtime_budget_rechecks_after_execution_policy_callback() {
    struct LaterPolicy {
        clock: Rc<Clock>,
        calls: Cell<u32>,
    }
    impl JobsPolicy for LaterPolicy {
        fn authorize(&self, _: &LocalPrincipal, action: JobAction) -> Result<(), PolicyFailure> {
            if action == JobAction::Execute {
                let old = self.calls.get();
                self.calls.set(old + 1);
                if old > 0 {
                    self.clock.advance(5);
                }
            }
            Ok(())
        }
    }
    let clock = Rc::new(Clock::new());
    let h = service(
        config(),
        Rc::new(LaterPolicy {
            clock: clock.clone(),
            calls: Cell::new(0),
        }),
        Rc::new(CounterProgram),
        clock,
    );
    let r = submit(&h, &who("a"), "k", 5).unwrap();
    assert_eq!(step(&h, &who("a"), &r.id).completed, 2);
    let stopped = step(&h, &who("a"), &r.id);
    assert_eq!(
        (stopped.state, stopped.completed, stopped.cancellation),
        (JobState::Cancelled, 2, Some(CancelReason::RuntimeBudget))
    );
}
