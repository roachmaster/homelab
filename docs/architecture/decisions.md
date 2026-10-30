# Framework architecture decisions

**Status:** Proposed decisions for the first framework iteration. Repository-specific reuse choices are tracked separately in the [reuse map](reuse-map.md).

| Decision | Why | Cost / rejection condition |
| --- | --- | --- |
| One logical header-routed ingress | Uniform admission and deterministic routing across REST/Kafka | The bridge must remain admission/dispatch only; reject domain logic growth |
| Explicit statically linked modules/starters | Predictable dependencies and simple Rust ownership; convenient opt-in assembly | Rebuild to add code; revisit dynamic loading only for a demonstrated operational need |
| Capability-first packages | Localize changes around business invariants instead of global technical layers | Some boundary mappings remain; avoid one crate per class/helper |
| Consumer-owned outbound ports | External technology does not define application vocabulary | Contracts must expose real failure/ownership/performance semantics, not hide them |
| Separate ingress contract and implementation | Adapters/bindings share a narrow admission conversation without importing concrete core logic | Keep this package narrowly owned; reject a general-purpose `common` dump |
| Separate framework and reference application | A reusable host can be tested without importing the demonstration domain | Two deliverables must be maintained; a second small capability proves the distinction |
| Transactional registration, frozen after startup | No duplicate winner by insertion order or half-applied registry | Hot registration is deferred; explicit restart is initially simpler |
| Conditional defaults, explicit overrides | Starter convenience without ambiguous dependency selection | Every override point needs documented precedence and conflict tests |
| Durable acceptance is route-specific | Prevent acknowledgment before work is safely recorded | A store/receipt protocol is required for durable routes; not all routes need it |
| At-least-once delivery plus bounded idempotency | Honest recovery model for retries and lost responses | Retention, conflicts, and external effects need explicit policies; no blanket exactly-once claim |
| One process first | Prove boundaries before adding network failures and operational cost | Split deployment only for measured independent scaling, isolation, or lifecycle needs |
| Feature-specific local adapters may start as modules | Avoid package explosion before reuse is demonstrated | Extract when enforcement, consumers, or independent evolution earns a crate |
| Tools, SDKs, ML engines, games, and storage engines remain optional | Keep kernel scope comprehensible | Specialized integrations need their own adapter/extension contracts |

## What is deliberately not included

No runtime plugin ABI, arbitrary reflection/code loading, universal repository abstraction, mandatory microservices, service mesh, scheduler platform, workflow DSL, IDE, code generator, model host, or custom database belongs in the initial kernel. None is inherently bad; each needs its own demonstrated consumer and acceptance tests.

## What would change these decisions?

- An actual independent release/isolation requirement could justify an out-of-process plugin protocol rather than a Rust ABI.
- Multiple independent consumers with the same stable adapter contract could justify extracting a new reusable crate.
- A measured throughput/latency limit could justify replication or partitioned processing, while retaining the same logical ingress contract.
- A business guarantee requiring durable external effects could justify an outbox/reconciliation design, not a claim that interface separation creates transactions.
- A first vertical slice that cannot add a second capability without editing bridge internals would invalidate the current extension design and require simplifying or changing it before wider adoption.

Record such decisions with the evidence, alternatives, compatibility impact, and verification needed. Do not turn an exploratory implementation detail into an irreversible framework contract by accident.
