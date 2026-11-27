#!/usr/bin/env python3
"""Context assembler for the design-driven generation prototype.

For a target card, build the smallest honest packet the generator needs:
the house rules, the slice of the domain glossary the card touches, the
target's own card in full, and for each collaborator ONLY its summary and
signature block (never an implementation). A collaborator with no card in
the store is silently absent from the packet; detecting that gap is the
generator's job (the NEED protocol), which is what the prototype tests.
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DESIGN = os.path.join(HERE, "design")


def load_store():
    cards = {}
    glossary = {}
    rules = []
    for name in sorted(os.listdir(DESIGN)):
        if not name.endswith(".json"):
            continue
        with open(os.path.join(DESIGN, name)) as f:
            data = json.load(f)
        if name == "glossary.json":
            glossary = data
        elif name == "rules.json":
            rules = data["rules"]
        else:
            cards[data["id"]] = data
    return cards, glossary, rules


def build_packet(target_id, cards, glossary, rules):
    card = cards[target_id]
    lines = []
    lines.append("# House rules")
    for r in rules:
        lines.append(f"- {r}")
    lines.append("")
    terms = [(t, glossary[t]) for t in card.get("glossary", []) if t in glossary]
    if terms:
        lines.append("# Domain language (only the terms this class touches)")
        for term, meaning in terms:
            lines.append(f"- {term}: {meaning}")
        lines.append("")
    lines.append(f"# Target: {card['id']} ({card['kind']}) -> write the file {card['file']}")
    lines.append(f"Summary: {card['summary']}")
    lines.append("Responsibilities:")
    for r in card["responsibilities"]:
        lines.append(f"- {r}")
    if card.get("invariants"):
        lines.append("Invariants:")
        for inv in card["invariants"]:
            lines.append(f"- {inv}")
    lines.append(f"Public signature to implement exactly:\n{card['signature']}")
    collabs = [c for c in card.get("collaborators", []) if c in cards]
    if collabs:
        lines.append("")
        lines.append("# Collaborator interfaces (signatures only, no implementations exist in this packet)")
        for cid in collabs:
            c = cards[cid]
            lines.append(f"## {c['id']}: {c['summary']}")
            lines.append(c["signature"])
    return "\n".join(lines) + "\n"


def approx_tokens(text):
    return round(len(text) / 4)


if __name__ == "__main__":
    cards, glossary, rules = load_store()
    target = sys.argv[1]
    packet = build_packet(target, cards, glossary, rules)
    print(packet)
    print(f"--- approx tokens: {approx_tokens(packet)} ---", file=sys.stderr)
