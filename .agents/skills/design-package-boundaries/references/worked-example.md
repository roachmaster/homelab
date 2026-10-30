# Worked example: publication capability

This is a conditional design example, not a prescribed template. Assume an application accepts a document over REST or Kafka through the same logical bridge, applies publication rules, stores it, and requests delivery. Storage and delivery technology vary independently of those rules. The parts may still run in one process and deploy together.

## Packages and compile-time dependencies

```text
publication_app             # composition / executable
  -> ingress_contract       # registry and admission-policy contracts
  -> ingress_bridge         # constructs bridge and explicit route registry
  -> publication_handler    # registers publication route/version -> handler
  -> publication
  -> publication_http
  -> publication_kafka
  -> publication_postgres
  -> publication_delivery

publication_http            # thin driving adapter
  -> ingress_contract       # canonical envelope, ingress port and outcomes
  -> HTTP framework

publication_kafka           # thin driving adapter
  -> ingress_contract       # same canonical envelope and ingress port
  -> Kafka client

ingress_bridge              # admission + dispatch, not domain orchestration
  -> ingress_contract       # implements ingress port; invokes registered handler port

ingress_contract            # owned by ingress/admission boundary maintainers
  public: Envelope, Header, Ingress, Handler, AdmissionPolicy, Outcome
  no transport, concrete handler or business-package dependencies

publication_handler         # registered inward-facing handler binding
  -> ingress_contract       # implements Handler; translates outcome
  -> publication            # decodes versioned body contract into PublishRequest

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

Arrows mean imports/build dependencies, not runtime calls. `ingress_bridge` receives handler implementations and a trusted admission-policy implementation through explicit construction. Neither the bridge nor the ingress adapters import `publication_handler` or `publication`. The registry is local to bridge construction, not a global service locator; duplicate route/version registrations fail assembly rather than silently choosing a winner. The core invokes an injected `StorePublication` at runtime without importing `publication_postgres`.

Runtime flow is distinct:

```text
REST controller ----\
                     -> Envelope -> shared Ingress -> admission + registry dispatch
Kafka consumer -----/                                -> publication handler
                                                       -> PublishDocument
                                                          -> storage / delivery
```

Fan-out is owned by the admitted application workflow, not duplicated in REST/Kafka adapters or hidden in the bridge. Other registered routes may target other capability handlers without growing bridge business logic. Replicas may implement this same logical path; no singleton thread or central server is implied.

`ingress_contract` owns only the versioned admission conversation shared by these actual consumers. It is not `common-core`: domain DTOs and driven ports remain owned by `publication`. The handler binding may be a boundary module instead of a build artifact if enforcement permits, but its dependencies must not introduce a bridge/core cycle. Composition knows implementations solely to assemble them; it must not perform business workflows.

## API, routing and data decisions

- An illustrative canonical header might express destination `publication.publish` and contract version `1`. These are logical allowlisted identifiers, not a method/class to execute; names and representation are example-only. Header schema/version identity must also be explicit where independently versioned. Neither adapter determines this destination by inspecting the document body.
- A configured REST method/path mapping and Kafka topic mapping can supply the same canonical destination/version. Define whether a supplied header must match that mapping, and reject conflicts. Both adapters submit the same logical envelope to `Ingress`; neither calls `PublishDocument` directly.
- The bridge validates admission, resolves exactly one supported registered destination/version, and checks route authorization against trusted identity/context. Unknown routes or unauthorized requests do not reach the handler. Domain/resource-specific authorization remains in `publication`; a route is never authentication.
- The registered handler translates the envelope body contract to `PublishRequest`, which is an application command, not an HTTP body with status codes or a database record. Domain validation remains in `publication`. Avoid duplicate representations without a semantic need.
- Preserve defined correlation, deadline and idempotency metadata through admission and the handler contract. Define how they affect execution and retry, not merely how they are copied. Adapter redelivery must not reset these semantics.
- `StorePublication` exposes the publication operation and guarantees the use case actually needs; it does not reproduce database CRUD operations. The adapter owns SQL and maps database errors to core-owned categories.
- `RequestDelivery` expresses delivery intent. Vendor response types, tokens, and retry headers do not become domain objects.
- Map controlled rejection/failure outcomes to HTTP responses or Kafka delivery actions in the corresponding adapter. Specify acknowledgments, offset commits, retryable failures and poison-message/dead-letter treatment; acknowledge only at the contract's defined completion or durable-handoff point, not merely because header validation succeeded. Neither protocol detail belongs in the bridge or domain core.

## Evidence for this shape

Use unit tests to show equivalent REST/Kafka logical requests reach the same bridge contract, including preserved metadata and conflicting mappings. Independently test registry uniqueness, deterministic routing, authorization and controlled rejection without handler effects. Test publication rules and business idempotency separately. Real REST/Kafka integration checks establish response mapping, broker redelivery/offset behavior and shutdown; adapter mocks cannot establish those guarantees. Enforce forbidden adapter-to-handler and bridge-to-concrete-core imports.

## The transaction question the graph cannot answer

If storage succeeds and delivery fails, the publication may already exist. Decide whether the public operation reports “stored, delivery pending,” fails with a recoverable stored identifier, or requires another explicitly defined outcome. Define retry/idempotency behavior before implementing it. Never promise “nothing changed” without evidence.

If durable delivery intent must be atomic with persistence, a transactional outbox may be appropriate: one storage-owned operation commits publication plus delivery intent, and a worker dispatches later. That changes the port contract and consistency model; it is not a reason to add an outbox to every feature. Alternatively, a simpler explicit retry workflow may satisfy the requirement. Match the design to the guarantee.

## Why these splits, and when to keep less structure

| Decision | Evidence that earns it | Reason to avoid or defer it |
| --- | --- | --- |
| Separate REST/Kafka adapters | Swap transports without altering admitted workflows; isolate different delivery lifecycles | Internal modules may suffice, but neither may bypass shared ingress |
| Shared ingress contract and bridge | Skill convention of one admission path with header-directed dispatch | Keep implementation small; this does not justify a distributed gateway or business god-class |
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

