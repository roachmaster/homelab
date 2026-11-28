# forge-usage

Provider usage accounting for Forge: bounded usage DTO validation and
canonical attempt accounting, the Codex/Muse metadata transports,
the native usage providers, the coalesced refresh collector, the
read-only usage store surface and the `ide.usage.*` API.

This crate is the Rust port of the deploy/ Python usage modules
(usage_types, usage_rpc, usage_muse, usage_provider, usage_collector,
usage_store, usage_api, work_quota_diagnostics). It performs no
generation, login or scraping: provider allowance comes only from the
pinned native metadata channels, and every payload is validated
against the strict contracts before it is stored or served.
