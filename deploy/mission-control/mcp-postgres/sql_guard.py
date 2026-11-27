"""Defense-in-depth check that a query is one read-only statement.

This is not the security boundary. The boundary is the dedicated SELECT-only
role, a READ ONLY transaction that is always rolled back, and the statement
timeout. The guard rejects obvious misuse early with a clear message.
"""

import re

MAX_SQL_CHARS = 20000
ALLOWED_FIRST = frozenset({"select", "with", "values", "table", "explain", "show"})
# Statement-level commands cannot start an accepted statement, so only words that
# can modify data inside SELECT/WITH/EXPLAIN need to be listed here.
DENIED_WORDS = frozenset({"insert", "update", "delete", "merge", "into"})
DENIED_FUNCTIONS = frozenset({
    "pg_sleep", "pg_sleep_for", "pg_sleep_until", "pg_read_file", "pg_read_binary_file", "pg_ls_dir",
    "pg_stat_file", "pg_ls_logdir", "pg_ls_waldir", "pg_ls_tmpdir", "pg_ls_archive_statusdir",
    "lo_import", "lo_export", "lo_get", "lo_open", "lo_creat", "lo_create", "lo_unlink", "lo_put",
    "lo_from_bytea", "dblink", "dblink_exec", "dblink_connect", "pg_terminate_backend",
    "pg_cancel_backend", "set_config", "pg_reload_conf", "pg_rotate_logfile", "nextval", "setval",
    "pg_advisory_lock", "pg_advisory_xact_lock", "pg_advisory_lock_shared", "pg_try_advisory_lock",
    "pg_advisory_xact_lock_shared", "pg_try_advisory_xact_lock", "query_to_xml",
    "query_to_xml_and_xmlschema", "query_to_xmlschema", "cursor_to_xml", "database_to_xml",
    "database_to_xml_and_xmlschema", "schema_to_xml", "pg_notify", "txid_current", "pg_current_xact_id",
    "pg_logical_emit_message", "pg_switch_wal", "pg_create_restore_point",
})
WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_$]*")


class GuardError(ValueError):
    """The SQL text is not accepted as a single read-only statement."""


def strip_literals(sql):
    """Return ``sql`` with comments removed and literals/quoted identifiers blanked.

    Raises GuardError on unterminated quotes or comments.
    """
    out = []
    index, length = 0, len(sql)
    while index < length:
        char = sql[index]
        pair = sql[index:index + 2]
        if pair == "--":
            end = sql.find("\n", index)
            index = length if end < 0 else end
            out.append(" ")
        elif pair == "/*":
            depth, index = 1, index + 2
            while depth:
                if index >= length:
                    raise GuardError("unterminated block comment")
                if sql.startswith("/*", index):
                    depth, index = depth + 1, index + 2
                elif sql.startswith("*/", index):
                    depth, index = depth - 1, index + 2
                else:
                    index += 1
            out.append(" ")
        elif char in "'\"":
            escaped = char == "'" and index > 0 and sql[index - 1] in "eE" and (
                index == 1 or not (sql[index - 2].isalnum() or sql[index - 2] == "_"))
            index += 1
            while True:
                if index >= length:
                    raise GuardError("unterminated quoted text")
                if escaped and sql[index] == "\\":
                    index += 2
                    continue
                if sql[index] == char:
                    if sql.startswith(char * 2, index):
                        index += 2
                        continue
                    index += 1
                    break
                index += 1
            out.append(" ''" if char == "'" else ' "q" ')
        elif char == "$":
            match = re.match(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$", sql[index:])
            if match and not (index and (sql[index - 1].isalnum() or sql[index - 1] == "_")):
                tag = match.group(0)
                end = sql.find(tag, index + len(tag))
                if end < 0:
                    raise GuardError("unterminated dollar-quoted text")
                index = end + len(tag)
                out.append(" '' ")
            else:
                out.append(char)
                index += 1
        else:
            out.append(char)
            index += 1
    return "".join(out)


def check_read_only(sql):
    """Return the stripped statement, or raise GuardError."""
    if not isinstance(sql, str) or not sql.strip():
        raise GuardError("query is empty")
    if len(sql) > MAX_SQL_CHARS:
        raise GuardError(f"query exceeds {MAX_SQL_CHARS} characters")
    if "\x00" in sql:
        raise GuardError("query contains a NUL character")
    bare = strip_literals(sql)
    body = bare.rstrip().rstrip(";").rstrip()
    if ";" in body:
        raise GuardError("only one statement is allowed")
    words = [word.lower() for word in WORD.findall(body)]
    if not words or words[0] not in ALLOWED_FIRST:
        raise GuardError("only SELECT, WITH, VALUES, TABLE, EXPLAIN or SHOW statements are allowed")
    if words[0] == "explain" and "analyze" in words[:6]:
        raise GuardError("EXPLAIN ANALYZE is not allowed")
    denied = sorted(set(words) & DENIED_WORDS)
    if denied:
        raise GuardError(f"query uses a disallowed keyword: {denied[0].upper()}")
    if re.search(r"\bfor\s+(no\s+key\s+)?(update|share|key\s+share)\b", body, re.IGNORECASE):
        raise GuardError("row-locking clauses are not allowed")
    for name in re.findall(r"([A-Za-z_][A-Za-z0-9_$]*)\s*\(", body):
        if name.lower() in DENIED_FUNCTIONS:
            raise GuardError(f"function {name.lower()} is not allowed")
    return sql.strip()
