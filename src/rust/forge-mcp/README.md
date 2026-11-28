# forge-mcp

Mission Control MCP API — a typed, discoverable MCP (Model Context
Protocol) surface over the Forge runner dispatch API and read-only
workflow state, so agent clients integrate once instead of by hand.

**Status: design only.** This crate holds the design and the contract
skeleton (port trait, request/response sums, tool registry as data).
No server runs yet. See [DESIGN.md](DESIGN.md) for the full design:
capability, routes, wiring, auth, deployment, and phasing.
