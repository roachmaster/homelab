"""Machine-readable cause codes for failed attempts (failure-cause defect).

Every worker failure summary is a fixed, bounded literal chosen by the
failing component, so the cause code is derived from it at publish time and
stored on the failed event beside a bounded copy of the summary. Unknown
summaries map to ``agent_failed`` so a failed event never lacks a cause.
"""

FAILURE_CAUSES = frozenset({
    "malformed_output",     # the agent's machine output could not be parsed
    "policy_unavailable",   # pinned execution policy/boundary missing or changed
    "context_unavailable",  # repository, revision, plan or context unavailable
    "resource_bound",       # declared stream/wall-time/launcher bound exceeded
    "agent_input",          # agent input could not be delivered
    "agent_failed",         # any other agent/tool failure
})

CAUSE_SUMMARY_BYTES = 512

_RULES = (
    ("resource_bound", ("resource bound", "did not exit within its bound")),
    ("malformed_output", ("malformed", "returned an invalid", "returned invalid", "returned no readable",
                          "returned multiple", "changed conversation identity", "progress reports were malformed")),
    ("policy_unavailable", ("execution boundary", "execution policy", "pinned policy", "immutable policy",
                            "immutable grok execution policy", "configured coding program", "outer execution boundary",
                            "cannot bypass", "process scope is unavailable", "supervision identity",
                            "session continuation", "ephemeral codex session", "cannot resume a codex session",
                            "lockfile")),
    ("context_unavailable", ("revision", "repository", "plan", "execution context", "project")),
    ("agent_input", ("input could not be delivered",)),
)


def failure_cause(summary):
    """Return the cause code for a failed attempt summary."""
    text = (summary or "").lower()
    for cause, needles in _RULES:
        if any(needle in text for needle in needles):
            return cause
    return "agent_failed"


def cause_summary(summary):
    """Bounded (UTF-8 bytes, whole codepoints) summary stored with the cause."""
    return (summary or "").encode("utf-8")[:CAUSE_SUMMARY_BYTES].decode("utf-8", errors="ignore")
