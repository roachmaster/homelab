# Design-driven generation prototype (forge-gpu)

Built 2026-11-26 to test Leonardo's thesis: if software is designed with
UML/CRC-style cards and domain-driven language, a small local model can
build each class from a compact packet of design, because the design
carries the decisions. The open problem he named is memory: assemble and
compact exactly the context a file needs, and let the model ask for more
context instead of guessing.

## Pieces

- `design/` — the design store. One small JSON card per class (summary,
  responsibilities, collaborators, invariants, exact public signature),
  plus `glossary.json` (domain language) and `rules.json` (house rules).
  Ten files, ~5.5 KB total.
- `assemble.py` — the context assembler. A packet is: house rules, the
  glossary terms the target touches, the target's card in full, and for
  each collaborator ONLY its summary and signature. No implementations.
- `generate.py` — sends packets to the local llama-server
  (Qwen2.5-Coder-7B) with an output protocol: the file, or one `NEED:`
  line. A NEED-only answer gets one reply from the store (a standing
  clarification that the interfaces are provided and implementations are
  withheld on purpose), then the model generates.
- `repair.py` — one repair round: packet + current file + compiler
  errors for that file, model returns the corrected file.
- `out/` — the generated crate (`house-animals`) and the exact packets
  the model saw (`out/packets/`).

## Result on the animal example (2026-11-26)

- Packets ran ~125 to ~600 tokens per file.
- Round 1: the model answered NEED on 4 of 7 files, asking for
  interfaces that were already in its packet (over-caution). The store's
  answer unblocked all four. Once (factory) it emitted NEEDs and then
  generated anyway.
- First compile: 9 errors, all last-mile slips (missing derives,
  missing trait imports). Signatures, delegation, singleton semantics,
  the macro and the client behavior were all structurally right.
- Repair rounds fixed the crate (provider needed the store answer once
  more; main needed two rounds because a parse error elsewhere had
  masked its errors). Final state: compiles and runs correctly — same
  dog behind both handles (rename seen through both), two distinct cats.
- The genuine-gap test (a `kennel` card referencing a `feeding_schedule`
  collaborator with no card): the model emitted NEEDs AND invented a
  method (`animal.next_feeding()`) that exists nowhere. Lesson: a small
  model will not reliably stop at a real gap, so missing-card detection
  belongs in the assembler, which can check it deterministically before
  the model is ever called. The model's NEED is the second line of
  defense, not the first.

## LLM request timing and memory clearing (standing rule, 2026-11-26)

Leonardo's rule: time every request to the local model, and let the
harness decide when the model's memory gets cleared. Implemented in
`llm.py`, used by both `generate.py` and `repair.py`:

- Every request is timed (monotonic clock) and logged as one JSON line
  to `out/llm-log.jsonl`: label, elapsed seconds, prompt tokens,
  response size, finish reason, outcome.
- Budgets, from the observed baseline (~40 tok/s generation, packets of
  125–600 tokens, 1,400 output tokens max — a legitimate request ends
  well under a minute): **warn at 60s, abort at 120s**. Past 120s the
  model is looping, not working. (The harness previously allowed 300s.)
- Hallucination tripwires, checked mechanically on every response: a
  line repeated 6+ times in a row; `finish_reason == "length"`; a
  `use crate::<module>` naming a module with no card in the store (the
  kennel failure mode, caught deterministically).
- On a timeout or any tripwire: **clear the model's memory** — a
  best-effort llama-server slot erase, plus the harness restarting the
  target from the bare packet with no accumulated conversation — then
  retry **once**. A second failure marks the target BLOCKED and
  surfaces it to the operator. Never retry in a loop.

## Verdict

The thesis held. With the design carrying the decisions, the local 7B
produced a correct multi-file crate from packets smaller than a typical
chat prompt, and every failure mode that appeared is one the structured
loop (assembler checks, compiler feedback) can catch mechanically.
