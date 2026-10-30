# forge-os — a numerical experiment, not an operating or application runtime

## Decision

**Exclude from the framework kernel; retain as an optional experimental capability or portfolio case study.** The repository name is broader than its contents. At the inspected revision it contains small bitwise/matrix/angle libraries and code-generation templates, not an operating system, scheduler, application bootstrapper, networking stack, common ingress bridge, or module lifecycle manager. Keeping this work visible in homelab is compatible with keeping it out of every application's dependency graph.

**Snapshot:** default branch `main`, privately pinned revision, inspected 2026-10-30. The repository is private; this public-safe analysis contains no copied implementation or private source links. Exact pinned evidence is retained in the private inventory. Only `main` was returned by branch inventory. The complete tree contained no README, `AGENTS.md`, repository skill, or GitHub Actions workflow.

## Actual scope and package graph

The workspace lists three packages:

| Package | Observed implementation | Framework relevance |
|---|---|---|
| `bitwave-matrix` | Pure integer bit operations; a fixed-width one-hot identity table; a row-major generic matrix and floating-point matrix operations | Optional numerical utility, not infrastructure |
| `bitwave-angle` | Angle-table functions and an angular projection computed using `bitwave-matrix` | Optional experimental domain capability |
| `bitwave-bin` | Generated starter-style addition function and one trivial test | Scaffolding, not a binary-format subsystem |

The actual dependency graph is `bitwave-angle -> bitwave-matrix`, with a declared Serde dependency on the angle package. `bitwave-matrix` and `bitwave-bin` declare no third-party dependencies; `bitwave-bin` is independent. The inspected angle implementation does not use the declared serialization dependency. There is no connection to Forge's generic contracts or other service packages. **Evidence O1–O4.**

This separation is useful mainly as a negative architectural constraint: a Rust application framework does not need numerical research merely because it shares the Forge name. Capability-oriented packaging means dependencies follow real behavior and consumers, not branding or aspirational repository titles.

## Representative implemented flow

The projection entry point receives a 16-bit signal and requested bit count. It selects that many rows from a fixed 16-row identity table, computes each row's bit overlap with the signal, converts those activations to floating-point numbers, generates a uniformly spaced angle vector, constructs a row matrix and a column matrix, and takes their scalar dot product. A companion function prints the result. **O2–O3.**

That is working source-level numerical logic, not only an empty interface. The lower-level calculator also directly implements bitwise AND, population count, overlap count, and row-wise dot-product results. The generic matrix provides construction, indexed access, transpose, row/column vector conversion, and a scalar dot product; a separate implementation supplies elementwise multiplication, row summation, vector multiplication, and general matrix multiplication. **O2, O5–O6.**

The name “angular projection” is a reasonable description of the implemented weighted sum. The documentation's stronger “centroid” language is not established by the computation: it sums activated angles and does not divide by total activation or apply circular averaging. This report makes no claim about the experiment's scientific validity. If the mathematical interpretation matters to a future application, specify the intended model and test against independently calculated reference values rather than treating comments as proof.

## What can be reused, and what must stay optional

The reusable engineering pattern is modest but sound: small deterministic functions, a one-way dependency between numerical capabilities, and tests close to the library boundary. Those traits are good for a portfolio demonstration and a possible `numerics` or `signal` capability if a real homelab use case arises. They do not justify an additional mandatory framework package now.

The bit representation, fixed 16-bit identity, angular semantics, floating-point matrix operations, and printing helpers are all domain-specific. They should never enter a universal request envelope, application context, module registry, or internal event bus. The templates are development material, not an implemented runtime registration system. **O2–O7.**

In the proposed framework, an application could register a small numerical handler that calls these functions. REST and Kafka would still enter through their thin adapters and the same header-routed ingress bridge. The handler's result would remain a typed capability result; optional notifications could use the common internal fan-out mechanism. No dedicated microservice, transport-specific math router, or dynamic plugin ABI is required.

## Correctness and maintainability observations

**Input contracts are not hardened for untrusted ingress.** Requesting more than 16 identity rows panics; zero bit count is accepted by the identity slice, while the angle constant divides by the supplied count without a zero guard. These are source observations, not executed failure demonstrations. A network-facing capability would need bounded, fallible input validation before invoking these functions. **O2, O3.**

**Matrix invariants are public and unenforced.** Rows, columns, and data are public. Callers can construct inconsistent dimensions and storage. Construction multiplies dimensions without an explicit checked-size limit. Individual coordinate access computes a flattened index rather than separately checking each coordinate, so an invalid column can address a later row if the flattened index still falls inside the vector. This is a semantic validation problem even when Rust's bounds checking prevents memory unsafety. **O5, lines 23–60.**

**Several operations check only part of their documented shape.** Vector `inner_product` checks the shared dimension but not that the right-hand matrix has exactly one column. Scalar dot product checks matching inner dimensions but not that the left side has exactly one row and the right side one column. Extra rows or columns can therefore be silently ignored instead of rejected. If retained, use checked constructors and complete shape contracts with edge-case tests. **O5, lines 120–135; O6, lines 45–68.**

**Documentation exceeds implementation in one concrete place.** The matrix module describes its type as serializable, but the type derives debugging, cloning, and equality only, and the matrix package has no Serde dependency. Serialization is not implemented there. This is a useful example of why the new framework's portfolio documentation must separate existing capability from planned capability. **O1, O5.**

**Some apparent coverage is demonstration output.** Both angle test functions print sample tables or projections without asserting numerical expectations. They can catch a panic on those inputs, but not an incorrect yet printable result. The matrix tests contain real assertions, including a dimension-mismatch panic expectation. **O8–O9.**

## Verification and suggested disposition

Observed source contains **14 explicit Rust test functions**: eleven matrix integration tests, two angle demonstration tests, and the starter addition test. The matrix API also contains a documentation example. No tests were executed. No builds, scripts, generators, or benchmark runs were executed. The repository's shell entry point offers build and clean commands but no aggregate test/format/lint gate. GitHub Actions returned zero runs for the exact inspected SHA. These facts establish available test material, not a passing build or a correctness certification.

For homelab, present this as a bounded research experiment with its implemented model, limitations, and a future correctness checklist. Do not elevate it into the framework foundation. If a genuine numerical use case appears, first add asserted reference outputs, zero/maximum/oversized input tests, complete shape validation, and allocation limits; then consider an optional capability package. This preserves the user's work without creating another god-crate or an unnecessary service boundary.

## Source evidence index

All locations refer to the pinned revision; exact private permalinks are held in the private inventory.

| ID | Inspected location |
|---|---|
| O1 | `Cargo.toml:1–10`; `crates/{bitwave-angle,bitwave-bin,bitwave-matrix}/Cargo.toml` |
| O2 | `crates/bitwave-matrix/src/calculator.rs:16–42`; `identity.rs:13–49` |
| O3 | `crates/bitwave-angle/src/nyquist.rs:3–23`; `projection.rs:24–56` |
| O4 | `crates/bitwave-bin/src/lib.rs:1–14` |
| O5 | `crates/bitwave-matrix/src/numeric/matrix2d.rs:7–135` |
| O6 | `crates/bitwave-matrix/src/numeric/calculator.rs:16–93` |
| O7 | `templates/bitwave_angle/`; `templates/core/`; `forge-scripts.sh:1–15` |
| O8 | `crates/bitwave-angle/tests/test_nyquist_table.rs:25–31`; `test_projection.rs:3–14` |
| O9 | `crates/bitwave-matrix/tests/test_bitwave_calculator.rs:23–72`; `test_identity.rs:31–69`; `test_matrix_calculator.rs:17–91` |
