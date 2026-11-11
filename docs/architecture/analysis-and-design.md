# Object-oriented analysis, design and UML in Forge

Use analysis to decide what a capability must guarantee. Design allocates those guarantees to domain operations, contracts, adapters and owners. UML makes selected structural and behavioral decisions reviewable.

The workflow draws on the private Study Notes collection at revision `74c9214f92073ef06f90c048c0761922dfb47f90`. The Schach requirements and analysis chapters motivate use cases, alternate flows and domain roles. The design chapter motivates assigning responsibilities and reviewing both from requirements to design and from each interaction back to its requirement. The UML chapter distinguishes structural, behavioral and deployment views. The Forge translation below uses this repository’s typed ingress, task/client and provider contracts. [Schach study and source map](https://github.com/roachmaster/study-notes/blob/74c9214f92073ef06f90c048c0761922dfb47f90/studies/schach-2008/README.md)

| Work | Repo skill |
|---|---|
| Translate a use case into owned responsibilities and contracts | [Model a Forge module](../../.agents/skills/model-forge-module/SKILL.md) |
| Locate invariants and choose dependency direction | [Design package boundaries](../../.agents/skills/design-package-boundaries/SKILL.md) |
| Check success, rejection and effect order | [Trace a Forge command](../../.agents/skills/trace-forge-command/SKILL.md) |
| Model construction, ownership and acquisition | [Wire Forge providers](../../.agents/skills/wire-forge-providers/SKILL.md) |
| Deliver the contract to another application | [Expose a Forge capability](../../.agents/skills/expose-forge-capability/SKILL.md) |
| Draw and review the appropriate view | [Document Forge with UML](../../.agents/skills/document-forge-uml/SKILL.md) |

## Worked example — bounded local counter jobs

This trace uses the implemented behavior described in the local jobs contract. Keep the job’s state separate from a command's outcome and from the lifetime of the application that owns the job. [Local jobs contract](../../src/rust/forge-local/JOBS.md)

| Goal or invariant | Owner and operation | Observable check |
|---|---|---|
| An operator accepts a counter job for later execution | The admitted submit binding calls the jobs service. The service reserves capacity, prepares state and commits acceptance. | A valid submission returns a receipt. Preparation failure returns no acceptance and releases the reservation. |
| Only explicit stepping executes a quantum | The jobs engine applies policy and executes through `jobs.step`. | Submit, list and inspect leave the counter unadvanced. A permitted step advances it by a bounded quantum. |
| Cancellation respects execution state | The jobs engine owns both the job and its cancellation decision. | Queued cancellation prevents work. Running cancellation returns Requested. A later step may stop at a checkpoint or complete. |
| An intent can be replayed within its retention window | The jobs engine binds owner, key and request data to the retained receipt. | The same owner, key and total return the original receipt. A changed total conflicts. |

A package dependency view answers what imports what. A sequence follows `jobs.submit` through the terminal, typed bridge, registered binding and engine. An admission failure ends that sequence before the jobs service is called. A state view can expand the queued, running and terminal transitions. Record cancellation as a request while running and show the guard for the final outcome. Use a separate assembly sequence when the question is how the application publishes a ready jobs handle. [Forge diagram gallery](diagrams/mermaid/README.md)

Use this lifecycle excerpt to distinguish stored state from the reply to a command. The event rows cover cancellation and successful completion. The full contract also describes expiry and failures.

| Starting state | Event and condition | State afterward | Observable outcome |
|---|---|---|---|
| Queued | Owner cancels before the first step | Cancelled | `CancelledBeforeStart` |
| Running | Owner requests cancellation | Running with cancellation recorded | `Requested` |
| Running | A step observes cancellation and stops | Cancelled | Terminal snapshot |
| Running | A valid step finishes the target | Succeeded | Terminal result, including when completion wins after a cancel request |
| Terminal | Owner cancels again | Same terminal state | `AlreadyTerminal` |

To review a change, start with each goal or invariant in the table and locate the operation and acceptance evidence. Then start from each public operation and dependency in the design and find the requirement or existing contract that justifies it. This worked trace is documentation derived from the current contract and does not record a new execution.
