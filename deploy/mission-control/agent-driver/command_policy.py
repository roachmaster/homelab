"""Translate a small command grammar to fixed argv vectors for fixture tests."""

import re
import shlex
import subprocess
from pathlib import Path


class CommandError(ValueError):
    pass


def parse_command(command, files):
    if not command or len(command) > 512 or any(char in command for char in "\r\n\x00"):
        raise CommandError("Expected one command line")
    if any(char in command for char in "$`\\"):
        raise CommandError("Shell expansions and escapes are outside the fixture grammar")
    lexer = shlex.shlex(command, posix=True, punctuation_chars="|;&<>()")
    lexer.whitespace_split = True
    lexer.commenters = ""
    try:
        tokens = list(lexer)
    except ValueError as error:
        raise CommandError("Invalid command quoting") from error
    if tokens.count("|") > 1:
        raise CommandError("Only one pipe is supported")
    if "|" in tokens:
        split = tokens.index("|")
        if tokens[split + 1:] != ["uniq"]:
            raise CommandError("The supported pipe target is uniq")
        return [file_command(tokens[:split], files), ["/usr/bin/uniq"]]
    return [file_command(tokens, files)]


def file_command(tokens, files):
    if len(tokens) < 2 or tokens[-1] not in files:
        raise CommandError("Expected one fixture filename")
    name, *options, filename = tokens
    if name == "wc" and options in (["-l"], ["--lines"]):
        return ["/usr/bin/wc", "-l", "--", filename]
    if name == "sort" and options in ([], ["-u"], ["--unique"]):
        flags = ["-u"] if options else []
        return ["/usr/bin/sort", *flags, "--", filename]
    if name == "head" and len(options) == 2 and options[0] == "-n":
        if re.fullmatch(r"[1-9][0-9]?", options[1]):
            return ["/usr/bin/head", "-n", options[1], "--", filename]
    if name == "grep" and options:
        *flags, pattern = options
        if flags and flags[-1] == "--":
            flags.pop()
        if all(re.fullmatch(r"-[Fn]+", flag) for flag in flags) and re.fullmatch(r"[a-zA-Z0-9 ._:-]{1,80}", pattern):
            return ["/usr/bin/grep", *flags, "--", pattern, filename]
    if name == "cut":
        match = re.fullmatch(r"-d ?(,) -f ?([1-9])", " ".join(options))
        if match:
            return ["/usr/bin/cut", "-d", match[1], "-f", match[2], "--", filename]
    raise CommandError("Unsupported command or arguments")


def execute(commands, directory: Path, uid: int, gid: int):
    previous = b""
    for argv in commands:
        result = subprocess.run(
            argv, cwd=directory, input=previous, capture_output=True,
            user=uid, group=gid, extra_groups=[], timeout=5,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C", "HOME": "/nonexistent"},
        )
        if result.returncode != 0:
            return result.returncode, result.stdout.decode(errors="replace"), result.stderr.decode(errors="replace")
        previous = result.stdout
    return 0, previous.decode(errors="replace"), ""
