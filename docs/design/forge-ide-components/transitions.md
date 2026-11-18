# Current Forge IDE transitions

Reviewed 18 November 2026. Exact event/DTO names are defined by [protocol.rs](../../../src/rust/forge-ide-ui/src/protocol.rs) and accepted by [UiModel](../../../src/rust/forge-ide-ui/src/model.rs). This table summarizes the implemented path; it does not create additional public APIs.

| Trigger | Guard and accepted change | Effect / recovery |
| --- | --- | --- |
| Startup / Resume / Pair | Bounded input; correlated session/catalog accepted before protected state | One prepared session effect. One allowed project auto-opens; empty/denied catalog has explicit handling. |
| Node/project selection | Configured catalog membership; invalidate old report, source and context | Abort local ownership where possible. New inspection requires admitted context. |
| Inspect / Refresh | Valid selection, clock/deadline and no competing pending inspection | New request token; clear obsolete report. Busy repeat does not create a second request. |
| Inspection result | Matching pending token, target/project, bounds and expiry | Accept Ready/Partial; reject invalid or current failure safely. Unmatched late results are ignored. |
| Cancel | Pending ownership invalidated | Abort local waiting; remote task completion remains unknown. |
| Explorer query / mode | Accepted report and matching entry-name mode; query bounded in UTF-8 | Local projection only; query survives a mode switch and stale-mode edits are ignored. |
| Entry/declaration/problem activation | Current report and appropriate entry/declaration/diagnostic identity | Select and reveal context. A source read is separately prepared when required. |
| Source result / line page | Matching report, entry and request; exact inspected-content digest | Accept bounded document/window. Line paging is local. A changed file requires refresh. |
| Pane, Wrap, Raw/Display | Current visible/accepted content; presentation constraints | Main-thread focus/scroll/geometry only. Background views do not undo a manual pane switch. |
| Chat send / run poll | Current conversation, model selection and stable submission identity | Submit queued work; retry/reconcile with the same identity after response loss. Poll accepted run state; no token-stream claim. |
| History selection / retention / delete | Current saved ID and revision; backend-confirmed result | Page history/open by DAO. Default expiry, explicit long-term retention and confirmed deletion preserve correct draft ownership. |
| Mail open / compose / follow-up | Current thread revision/status/model profile and recall budget | Queue work and poll. Archive at recall exhaustion remains readable; unsupported mutations are not fabricated. |
| Propose / edit draft | Verified file context and bounded candidate/editor identity | Stage a diff or dirty textarea. Discard has no write effect; a dirty file switch requires resolution. |
| Save / Apply / Undo | Write-enabled managed project, current native revision, operation identity and fence | Native journaled publication. Confirmed receipt triggers verified refresh; conflict keeps review/draft; unknown outcome offers status reconciliation. |
| Home / Design | Checkpoint/handoff corresponds to latest acknowledged draft state | Same-tab navigation waits for save; bounded local JSON can recover supported drafts if remote restore is unavailable. |
| Docs view entry | Declared fragment after admitted session and checkpoint/Mail restoration | Dispatch existing Rust view action once, then consume URL fragment. Report IDs and credentials never become route state. |
| pagehide / fatal / session loss | Tear down the owned instance and close dispatch | Clear protected DOM synchronously, abort local work and recheck admission before restore. |

## Separate states and authority

Inspection Phase, selected screen, pending inference, Mail thread status, dirty document and operation receipt are different state machines. A Ready inspection does not prove compiler success; a Reply ready dot is not unread persistence; cancellation intent does not establish a terminal backend outcome.

Host session failures on configured stateful routes can trigger fail-closed recovery. Independently denied operations/projects retain their explicit bounded failure behavior. Do not infer authorization changes from prose; host and source agent remain independent gates.

The tab checkpoint, bounded Home/Design handoff and durable operator history have different lifetimes. Source bookmarks are resolved against a fresh report. Browser reload/back may recover supported state but is not a general browser offline cache or a durable cross-device Mail draft.

## Acceptance

`make ui-check` exercises actual WASM and deterministic generated adapters. `make ui-qa` retains 28 ordered groups with real-agent journeys and separately controlled fault/persistence cases. Cases cover repeated/stale events, local no-request actions, response loss, drafts, source revisions, dirty editors, managed write receipts, leave/return and protected-DOM clearing. New screenshots illustrate owned fixture states; they do not replace action assertions.
