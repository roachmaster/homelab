#!/usr/bin/env python3
"""Generator for the design-driven prototype.

For each target, in dependency order: assemble its packet, send it to the
local model with the output protocol (the file, or one NEED: line), and
write the result into out/src/.

Round 2: when the model answers NEED-only, the harness answers from the
design store. Everything the model can legitimately ask for here is a
collaborator interface, and those are already in the packet by design, so
the answer is a standing clarification: the interfaces are provided,
implementations are deliberately withheld, program against the
signatures. The model then generates with the same packet plus that
answer appended.
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from assemble import load_store, build_packet, approx_tokens
import llm

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "out")
API = llm.API
MODEL = llm.MODEL
KNOWN_MODULES = None  # set in main() from the store: file stems with cards

SYSTEM = """You are a code generator working from a design packet.
Output ONLY the complete Rust source file for the target: no markdown fences, no commentary, no explanation.
Implement the target's public signature exactly as given. Use collaborator interfaces exactly as given; do not invent methods, fields or types for them.
If anything you need (a type, an interface, a rule) is NOT in the packet, output exactly one line:
NEED: <what is missing>
and nothing else."""

ANSWER = """

# Answer to your context request
Everything you asked for is already in this packet, under "Collaborator interfaces".
Collaborator implementations are deliberately withheld: program against their signatures only.
Import the traits you call methods on. Generate the file now; do not repeat the NEED."""

CARGO_TOML = """[package]
name = "house-animals"
version = "0.1.0"
edition = "2021"
"""


def call_model(messages, label="generate"):
    """Timed call with Leonardo's clearing policy (see llm.py): on a
    timeout or a hallucination tripwire, clear the model's memory and
    retry ONCE from the bare messages; a second failure propagates."""
    try:
        return llm.timed_call(messages, label=label, known_modules=KNOWN_MODULES)
    except (llm.LlmTimeout, llm.LlmHallucination) as e:
        print(f"{label}: {e.kind} after {e.elapsed:.0f}s - clearing model "
              f"memory, one retry from the bare packet")
        llm.clear_memory(label)
        return llm.timed_call(messages, label=f"{label}:retry",
                              known_modules=KNOWN_MODULES)


def strip_fences(text):
    text = text.strip()
    m = re.match(r"^```[a-z]*\n(.*?)\n?```$", text, re.DOTALL)
    return m.group(1).strip() if m else text


def split_needs(text):
    """Split model output into (need_lines, code). Fences are stripped first."""
    text = strip_fences(text)
    needs, code = [], []
    for line in text.splitlines():
        if line.strip().startswith("NEED:"):
            needs.append(line.strip())
        else:
            code.append(line)
    return needs, "\n".join(code).strip()


def main():
    global KNOWN_MODULES
    targets = sys.argv[1:]
    cards, glossary, rules = load_store()
    KNOWN_MODULES = {os.path.splitext(c["file"])[0] for c in cards.values()}
    os.makedirs(os.path.join(OUT, "src"), exist_ok=True)
    os.makedirs(os.path.join(OUT, "packets"), exist_ok=True)
    with open(os.path.join(OUT, "Cargo.toml"), "w") as f:
        f.write(CARGO_TOML)
    for target in targets:
        card = cards[target]
        packet = build_packet(target, cards, glossary, rules)
        with open(os.path.join(OUT, "packets", f"{target}.txt"), "w") as f:
            f.write(packet)
        messages = [{"role": "system", "content": SYSTEM},
                    {"role": "user", "content": packet}]
        try:
            needs, code = split_needs(call_model(messages, label=f"gen:{target}"))
        except (llm.LlmTimeout, llm.LlmHallucination) as e:
            print(f"{target}: BLOCKED ({e.kind} after clear+retry) - "
                  f"surfaced, not retried further; see out/llm-log.jsonl")
            with open(os.path.join(OUT, f"NEED-{target}.txt"), "w") as f:
                f.write(f"blocked: {e.kind}: {e}\n")
            continue
        note = ""
        if needs and not code:
            messages.append({"role": "user", "content": packet + ANSWER})
            needs2, code = split_needs(call_model(messages, label=f"gen:{target}:need"))
            note = f"round1 NEED x{len(needs)}, answered from store; round2 NEED x{len(needs2)}"
            needs = needs2
        elif needs:
            note = f"hedged: NEED x{len(needs)} but generated anyway"
        if not code:
            print(f"{target}: STILL BLOCKED ({note}) needs={needs}")
            with open(os.path.join(OUT, f"NEED-{target}.txt"), "w") as f:
                f.write("\n".join(needs))
            continue
        with open(os.path.join(OUT, "src", card["file"]), "w") as f:
            f.write(code + "\n")
        print(f"{target}: wrote {card['file']} ({len(code.splitlines())} lines, packet ~{approx_tokens(packet)} tokens) {note}")


if __name__ == "__main__":
    main()
