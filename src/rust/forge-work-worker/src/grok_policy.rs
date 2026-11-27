//! The pinned Grok configuration documents (work_grok.py).
//! The supervisor byte-compares the staged files against these
//! constants; they are part of the execution policy identity.

pub const GROK_CONFIG: &str = "[cli]\nauto_update = false\nuse_leader = false\nsession_registry = false\n[session]\nload_envrc = false\n[features]\nshared_daemon = false\nauto_wake = false\nask_user_question = false\ncloud_agents = false\nbackend_tools = false\ntitle_refresh = false\ntelemetry = false\n[managed_mcps]\nenabled = false\ngateway_tools_enabled = false\n[memory]\nenabled = false\n[memory_v2]\nenabled = false\n[workflows]\nenabled = false\n[goal]\nenabled = false\n[compat.claude]\nhooks = false\nmcps = false\n[compat.codex]\nhooks = false\n[compat.cursor]\nhooks = false\nmcps = false\n[telemetry]\notel_enabled = false\ntrace_upload = false\n";

pub const GROK_MANAGED_POLICY: &str = "allow_managed_hooks_only = true\nallow_managed_mcp_servers_only = true\nallowed_mcp_servers = []\nenable_all_project_mcp_servers = false\nstrict_known_marketplaces = []\n";

pub const GROK_BINARY_SHA256: &str =
    "add4d201dbf2c0bd6077a36feb55a76939de30bae48dbb06f16b2307d38ceacb";
