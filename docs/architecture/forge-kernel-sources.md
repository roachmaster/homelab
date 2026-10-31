# Forge kernel plan source ledger

Checked **2026-10-31** for the [hosted-kernel plan](forge-kernel-plan.md).
These are public primary/maintainer sources. Summaries are paraphrases, not copied
book material. Source facts inform the design; none implements or endorses Forge.
The plan's phases, package choices and command names are our proposed synthesis.

| Source and verified link | Narrow factual grounding | How the plan uses it |
| --- | --- | --- |
| [MIT xv6 book, Operating system interfaces](https://mit-pdos.github.io/xv6-riscv-book/unix.html) | Describes processes/memory, files/I/O, kernel privileges/system calls and the shell as a user program. | Separate OS mechanisms from a hosted command interface; application jobs are not OS processes. |
| [Linux kernel, Control Group v2](https://docs.kernel.org/admin-guide/cgroup-v2.html) | Controllers organize process groups and distribute system resources. | Application queue limits are not OS-enforced isolation; do not build a scheduler/allocator to imitate them. |
| [Waldo, Wyant, Wollrath and Kendall, A Note on Distributed Computing, 1994](https://waldo.scholars.harvard.edu/publications/note-distributed-computing) | The authors distinguish remote interactions by latency, concurrency and partial failure rather than treating them as ordinary local objects. | Keep node selection and uncertain outcomes explicit; no transparent-location promise. |
| [AWS Builders' Library, Making retries safe with idempotent APIs](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/) | Describes caller request tokens, semantic equivalence, changed-intent rejection and late-arrival considerations. | Design scoped submission keys, bounded retention and restart/durability semantics; not a universal exactly-once claim. |
| [gRPC cancellation](https://grpc.io/docs/guides/cancellation/) | Application handlers must coordinate stopping their work; RPC cancellation generally cannot interrupt arbitrary handler execution. | Distinguish cancel requested, stopped and terminal outcome. Not a transport recommendation. |
| [gRPC deadlines](https://grpc.io/docs/guides/deadlines/) | Discusses explicit deadlines, server cleanup responsibility and remaining-time propagation across clock differences. | Separate wait budgets, job deadlines and observed completion; exact future transport mapping remains open. |
| [OWASP Authorization Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html) | Distinguishes authentication/authorization, recommends least privilege, default denial and checks on every request/resource. | Verify callers and target identity; authorize inspect/cancel/session operations, not just initial submission. |
| [Rust std::fs::Permissions](https://doc.rust-lang.org/std/fs/struct.Permissions.html) | Readonly/mode metadata does not determine effective access, including ACL/group/mount nuances. | Do not treat disk permission reporting as remote authorization or sandboxing. |
| [Raspberry Pi Linux kernel documentation](https://www.raspberrypi.com/documentation/computers/linux_kernel.html) | Distinguishes native/cross builds, model/architecture configurations and kernel-image installation. | Defer target-specific boot work; no hosted-library-to-bootable-OS claim. |

## Repository evidence

Baseline inspected: public homelab
[`1267205e1ee0af849bdefd347167836dab82bd28`](https://github.com/roachmaster/homelab/tree/1267205e1ee0af849bdefd347167836dab82bd28).
Relative links below refer to the working repository; the pinned baseline defines
what was reviewed for this plan.

- [Ingress source](../../src/rust/forge-ingress-contract/src/lib.rs): exact header fields, typed envelope, host-supplied admission context.
- [Bridge source](../../src/rust/forge-bridge/src/lib.rs): ordered checks, synchronous non-`Send`/`Sync` handler erasure, payload/context handler signature.
- [Task source](../../src/rust/forge-task/src/lib.rs) and [I/O client source](../../src/rust/forge-io-client/src/lib.rs): synchronous contracts, no built-in job runner or cancellation.
- [Bootstrap source](../../src/rust/forge-bootstrap/src/lib.rs) and [provider source](../../src/rust/forge-provider/src/lib.rs): typed consuming registration/freeze, explicit construction and acquisition.
- [Disk source](../../src/rust/forge-kernel/src/disk.rs) and [contract](../../src/rust/forge-kernel/README.md): native path/session semantics, no sandbox, unbounded materialized listing, no network codec or file mutation API.
- [Recorded verification](../../src/rust/README.md): previous implementation checks. Reading those records is not a fresh test execution.

No private repository source, private infrastructure addresses or credentials are
included. This pass adds documentation only; no device, network listener or
implementation was configured.
