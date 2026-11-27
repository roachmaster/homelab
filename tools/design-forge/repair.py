#!/usr/bin/env python3
"""One repair round: packet + current file + compiler errors -> model -> file."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from assemble import load_store, build_packet
import generate
import llm
from generate import strip_fences, SYSTEM

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "out")


def main():
    args = sys.argv[1:]
    store_dir = generate._flag(args, "--store", os.path.join(HERE, "design"))
    out_dir = generate._flag(args, "--out", OUT)
    target, errors_path = args[0], args[1]
    llm.set_out_dir(out_dir)
    cards, glossary, rules = load_store(store_dir)
    card = cards[target]
    packet = build_packet(target, cards, glossary, rules)
    path = os.path.join(out_dir, "src", card["file"])
    with open(path) as f:
        current = f.read()
    with open(errors_path) as f:
        errors = f.read()
    user = (packet
            + "\n# Your current file for this target (it has compiler errors)\n```rust\n"
            + current
            + "```\n# Compiler errors for this file\n"
            + errors
            + "\nOutput ONLY the complete corrected file for the target.")
    cards_modules = {os.path.splitext(c["file"])[0] for c in cards.values()}
    generate.KNOWN_MODULES = cards_modules
    raw = generate.call_model([{"role": "system", "content": SYSTEM},
                               {"role": "user", "content": user}],
                              label=f"repair:{target}")
    code = strip_fences(raw)
    with open(path, "w") as f:
        f.write(code + "\n")
    print(f"{target}: repaired ({len(code.splitlines())} lines)")


if __name__ == "__main__":
    main()
