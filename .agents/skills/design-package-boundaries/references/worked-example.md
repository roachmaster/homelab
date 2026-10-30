# Worked example: publication capability

This is a conditional design example, not a prescribed template. Assume an application accepts a document over HTTP, applies publication rules, stores it, and requests delivery. Storage and delivery technology vary independently of those rules. The parts may still run in one process and deploy together.

## Packages and compile-time dependencies

```text
publication_app             # composition / executable
  -> publication
  -> publication_http
  -> publication_postgres
  -> publication_delivery

publication_http            # driving adapter
  -> publication            # application API and results
  -> HTTP framework

publication_postgres        # driven adapter
  -> publication            # StorePublication contract and owned values
  -> database driver

publication_delivery        # driven adapter
  -> publication            # RequestDelivery contract
  -> delivery vendor SDK

publication                 # capability core; no imports of the packages above
  public: PublishDocument, PublishRequest, PublishOutcome
  ports: StorePublication, RequestDelivery
  internal modules: rules, publish_workflow, values
```

Arrows mean imports/build dependencies, not runtime calls. The use case invokes an injected `StorePublication` implementation at runtime; the `publication` package still does not import `publication_postgres`.

Port contracts live with their consuming capability. If the language requires external adapters to implement public contracts, expose only those needed types; keep unrelated internals private. The composition root is allowed to know concrete adapters because its job is to assemble them. Its knowledge must not leak back into the core.

## API and data decisions

- `PublishRequest` is an application command, not an HTTP body with status codes or a database record. The HTTP adapter parses transport fields and maps them to the command. Domain validation remains in `publication`.
- `StorePublication` exposes the publication operation and guarantees the use case actually needs; it does not reproduce all database CRUD operations. The adapter owns SQL and maps database errors to core-owned categories.
- `RequestDelivery` expresses delivery intent. Vendor response types, tokens, and retry headers do not become domain objects.
- Avoid four identical `Document` records simply to satisfy a layer diagram. Map when invariants, units, versions, lifetime, or representation differ.

## The transaction question the graph cannot answer

If storage succeeds and delivery fails, the publication may already exist. Decide whether the public operation reports “stored, delivery pending,” fails with a recoverable stored identifier, or requires another explicitly defined outcome. Define retry/idempotency behavior before implementing it. Never promise “nothing changed” without evidence.

If durable delivery intent must be atomic with persistence, a transactional outbox may be appropriate: one storage-owned operation commits publication plus delivery intent, and a worker dispatches later. That changes the port contract and consistency model; it is not a reason to add an outbox to every feature. Alternatively, a simpler explicit retry workflow may satisfy the requirement. Match the design to the guarantee.

## Why these splits, and when to keep less structure

| Decision | Evidence that earns it | Reason to avoid or defer it |
| --- | --- | --- |
| Separate HTTP adapter | Public protocol evolves independently; core tests should not boot a server | A tiny one-off program may use an internal adapter module while retaining the import boundary |
| Separate SQL adapter | Driver types, persistence evolution, real database tests, independent dependency constraints | No persistent storage exists; a speculative repository adds nothing |
| Keep rules and workflow in `publication` | One cohesive capability, shared vocabulary, no independent consumers | Split later if independent policy consumers or dependency constraints actually emerge |
| Separate a domain package | Stable domain model used by multiple real application capabilities | Do not split only because a textbook diagram has a `domain` box |
| Share a contract package | Multiple consumers need a stable versioned contract with clear ownership | Do not move every DTO and port into `shared`; it becomes the highest-coupled package |
| Separate deployment | Independent scaling, availability, security, or release needs justify operational costs | A code boundary alone is insufficient evidence for a service |

In a larger system, `billing`, `catalog`, and `publication` are separate capabilities, not subfolders in an all-knowing application core. Their adapters can be named or nested by capability according to build tooling. Folder spelling is secondary to ownership, public contracts, and enforced edges.

## Example review finding

```text
High — publication/publish imports SqlClient and returns VendorDeliveryError.
Evidence: cite the actual imported symbols and the public return type in the repository.
Consequence: storage and vendor changes propagate into the use case and callers;
core tests require infrastructure setup and cannot exercise failure policy cheaply.
Smallest correction: define the required storage/delivery conversations in publication,
move technology translation into adapters, and keep application outcomes core-owned.
Keep: publication rules and orchestration together; no evidence supports another package.
Verification: policy tests with controlled failures; real adapter tests for missing rows,
vendor rejection, and duplicate requests; one production-wiring integration path.
```

This example is not evidence of a defect in any real repository. A real finding needs actual files/symbols and the existing behavioral contract.
