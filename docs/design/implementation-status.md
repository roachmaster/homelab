# Current Forge implementation

Reviewed 18 November 2026 against the repository after `7e03991`. This is the current design reference. Earlier proposal PDFs, review reports and schematic diagrams remain dated design history. A capability implemented in the repository is not automatically enabled by a running host.

## Portal and workspace

| Surface | Implemented behavior | Boundary or remaining work |
| --- | --- | --- |
| Home | `/home` launches the IDE, Studio and Room One; compact documentation links lead to the Design hub. The Forge name returns here in the same tab. | A launcher link does not establish backend readiness. |
| Design | `/design` is an index. Focused readers have shared Forge navigation, sticky topics and a Design breadcrumb. Previous chats has a local sandbox with Reset. | The sandbox uses sample data and does not mutate saved conversations. |
| Chat | Compact thread-title history drawer, new-chat icon, open message stream, inline composer, model selection, saved history search and retention management. | Search covers the loaded history page. Model choices come from the admitted catalog. |
| Mail | Inbox/Archive, sender and date metadata, bounded previews, Compose, thread headers, safe Markdown and a quick Reply action. | Reply ready is a generation status, not a persistent unread flag. Archive occurs at the recall limit; manual archive and delete mutations are not implemented. |
| Workbench | Header project breadcrumb and view sheet; unified search with entry-name mode; Explorer, verified File source, Outline and supplied Problems. Markdown has Raw/Display and source has Wrap. | Search covers the accepted report, not arbitrary filesystem contents or resolved references. Problems are inspection diagnostics, not compiler/test results. |
| Editing | Contextual questions, bounded edit proposals/diffs, direct textarea editing, Apply, Undo and status reconciliation are implemented for admitted managed projects. | Writes require an opted-in managed workspace, independent agent authorization and revision/journal checks. A rendered button is not write authority. |

The current [implementation captures](/design/wireframes) use disposable acceptance fixtures. Some fixtures deliberately disable Mail, contain partial inspection results, or mask editable controls. They are current generated UI, not captures of private operator data.

## State, storage and inference

| Concern | Implemented ownership | Practical limit |
| --- | --- | --- |
| Browser application | `forge-ide-ui` Rust/WASM owns domain state, event guards and prepared effects. Generated main JS renders, handles focus and owns browser lifecycle. Generated worker JS runs admitted I/O and returns results. | The browser ABI is **4**; deploy HTML/CSS/JS/WASM as one verified bundle. The HTTP protocol remains version 1. |
| Same-tab navigation | The IDE saves a checkpoint and bounded local JSON handoff before Home/Design. Return resumes admission, restores state and re-verifies Workbench source. Declared `#chat`, `#mail` and `#workbench` entry actions run after restore and are consumed. | URLs contain view intent, not reports, credentials or source identities. A fresh tab does not inherit another tab's unsent drafts. |
| Chat persistence | PostgreSQL keeps operator history, tab checkpoints, conversation retention and queued runs. The history DAO supports list/open, expiry, long-term retention and deletion. | A default expiry applies automatically; deleting a saved conversation is distinct from cancelling a model run. |
| Mail persistence | PostgreSQL stores threads, ordered messages, queued runs and recall metadata. Each inference reconstructs the accepted transcript and pins its model/profile. | Mail drafts survive the supported same-tab documentation handoff in local JSON; there is no durable cross-device Mail-draft table. |
| Dispatch | Mission Control commits queued work, events and notifications. A runner claims work and performs private inference; browser polling reads accepted state. | PostgreSQL notifications wake readers; committed rows are authoritative. This is not browser push, streaming tokens or model process memory. |
| Mail token budget | Repository and inspected live runner profile: **4,096 context / 1,024 completion / 16 safety margin / 256 minimum follow-up tokens**. | An 8k profile is not active in the inspected runner. Context reservation is not a guarantee of response length. |
| Recall/cache | The complete selected thread is re-tokenized within its profile. Mail can request llama.cpp prefix-cache reuse; the database remains the transcript authority. | Prefix caching is an optimization, not persistent model memory. A full thread archives for review when follow-up no longer fits. |

Read the focused [Chat history](/design/chat-history), [dispatch](/design/chat-dispatch), [Mail](/design/mail) and [workspace recovery](/design/workspace) contracts for the exact DAO/DTO, event and recovery flows.

## Framework and game capabilities

| Area | Implemented repository slice | Remaining boundary |
| --- | --- | --- |
| Plug-and-play | Typed configuration and registrars stage factory recipes; bootstrap validates/freezes before creation, then injects ready ports or scoped providers. Singleton, Transient and borrow-tied leases have distinct lifetimes. | Consumers do not access a global registry. Pure values and reducers do not need empty registrars. |
| Durable jobs/access | `forge-jobs` and `forge-access` provide authenticated contracts, retained receipts, leases, retry/recovery and owned execution. | This Rust jobs capability is distinct from the Python/PostgreSQL Chat/Mail runner and Studio service. |
| Game runtime | Fixed-tick Q16 simulation, controller/collision queries, movement/jump, animation poses, bounded snapshots, debug traces and replay are implemented. | Room One is a browser playtest, not a complete general-purpose game editor. |
| Assets | Offline cooking, manifests, atlas scale, verified level loading, two authored rooms and replay resource admission are implemented. | Browser I/O workers return bytes; Rust verifies and owns simulation activation. |
| Studio | Generated Studio UI and a Python/PostgreSQL static-web build service implement admission, idempotency, worker leases, cancellation and verified previews. | Studio's current browser controller is generated JavaScript; moving its application intent into Rust/WASM remains work. Native/desktop/mobile game export is not implied. |

See [plug-and-play](/design/forge-providers), [workers](/design/forge-workers), [game runtime](/design/game-studio), [physics](/design/game-physics) and [assets](/design/game-asset-pipeline).

## Observed deployment checkpoint

Read-only checks on 18 November 2026 found the local web host, source agent and Chat/Mail runner active. The web host uses the trusted-proxy origin `https://portal.thedevex.com` and listener `127.0.0.1:8788`. The checked host has **no Studio relay configured**. Its source agent has the `homelab` project without managed-write opt-in. Studio and managed edits are therefore repository capabilities, not enabled live-portal operations at this checkpoint.

The live source agent is an older deployed release; repository improvements do not update it automatically. GPU driver/model residency and remote hardware observations have not been requalified by this documentation pass. The service inventory keeps those earlier observations dated separately.

## Source and verification

Implementation authority is the code and its configuration, not the older diagrams:

- [UI declarations and transport map](https://github.com/roachmaster/homelab/blob/main/src/rust/forge-ide-web/src/generate/application.rs), [Rust UI model](https://github.com/roachmaster/homelab/blob/main/src/rust/forge-ide-ui/src/model.rs), [Workbench guide](https://github.com/roachmaster/homelab/blob/main/src/rust/forge-ide-web/WORKBENCH.md).
- [Native host](https://github.com/roachmaster/homelab/tree/main/src/rust/forge-ide-web-host/src), [managed agent](https://github.com/roachmaster/homelab/tree/main/src/rust/forge-agent/src).
- [Chat/Mail runner](https://github.com/roachmaster/homelab/tree/main/deploy/mission-control/agent-runner), [Mail profile](https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/mail_tokenizer.py), [Studio service](https://github.com/roachmaster/homelab/tree/main/deploy/mission-control/studio).
- [Framework roadmap](https://github.com/roachmaster/homelab/blob/main/docs/roadmap/forge-framework.md), [service inventory](https://github.com/roachmaster/homelab/blob/main/docs/architecture/forge-ide-system-inventory.yaml).

Native/model tests, actual-WASM checks, controlled worker faults and sandboxed browser journeys establish different evidence. Captures illustrate fixture states; they do not establish physical Safari behavior, production credential rotation, or successful execution of an unconfigured service. Fresh validation results belong to the change/release record rather than a permanent claim that every future revision passes.
