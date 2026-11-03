# Design and browser skill adaptations

Reviewed and adapted 2026-11-03 against homelab
[`a92457b3e9d4ddad21efe0b1433a6abe3fbedc5e`](https://github.com/roachmaster/homelab/tree/a92457b3e9d4ddad21efe0b1433a6abe3fbedc5e).
These are **repository-local text adaptations**, not unmodified upstream skill
installations. No upstream executable, hook, launcher, asset library, generated
frontend, MCP configuration or service is included or enabled.

## Where the guidance lives

- [Design Forge IDE UX](../../.agents/skills/design-forge-ide-ux/SKILL.md) keeps
  its existing interaction/state scope and adds
  [workbench craft](../../.agents/skills/design-forge-ide-ux/references/workbench-craft.md).
- [Design Forge UI](../../.agents/skills/design-forge-ui/SKILL.md) keeps the
  YAML/typed Rust generation and thin hexagonal boundaries; its
  [motion reference](../../.agents/skills/design-forge-ui/references/motion.md)
  adapts animation guidance without importing a frontend framework.
- [Test Forge UI](../../.agents/skills/test-forge-ui/SKILL.md) adds a discoverable
  workflow around the existing 18-group browser harness, not another QA stack.

The upstream ideas below are rewritten for Forge's read-only professional IDE.
Application HTML/CSS/JS remains generated from validated declarations. Typed Rust
owns application policy in worker/WASM; generated browser adapters remain thin;
native host/agent admission and capability ports remain independent. Missing
generator support is named explicitly rather than hidden in raw code strings.

## Pinned sources, licenses and changes

| Source | Reviewed material | Local adaptation and deliberate exclusions |
| --- | --- | --- |
| [VoltAgent/awesome-design-md](https://github.com/VoltAgent/awesome-design-md/tree/fcc70284932276d085f01ef9f83edde90b1258cb) · [MIT](skill-licenses/voltagent-MIT.txt) | [README/index](https://github.com/VoltAgent/awesome-design-md/blob/fcc70284932276d085f01ef9f83edde90b1258cb/README.md), [Cursor entry](https://github.com/VoltAgent/awesome-design-md/blob/fcc70284932276d085f01ef9f83edde90b1258cb/design-md/cursor/README.md) | Treat design analyses as reference material and record precisely what was inspected. Do not import the collection, brand assets or marketing layouts. The inspected Cursor entry redirects to getdesign.md; that external content was not reviewed or vendored and is not pinned by the Git revision. |
| [Leonxlnx/taste-skill](https://github.com/Leonxlnx/taste-skill/tree/12af471f627f866411b4bd785b19646d3672784c) · [MIT](skill-licenses/taste-skill-MIT.txt) | [Skill](https://github.com/Leonxlnx/taste-skill/blob/12af471f627f866411b4bd785b19646d3672784c/skills/taste-skill/SKILL.md), [app screens](https://github.com/Leonxlnx/taste-skill/blob/12af471f627f866411b4bd785b19646d3672784c/skills/taste-skill/references/app-screens.md) | Ground hierarchy in the user's real job/data, avoid generic dashboard decoration and check fresh rendered views. No random style dealing, asset fetching, CSS/JS starter, fixed color budget or mandatory font. The cited desktop study excludes dense operations tables and loading/error/phone screens; its preferences are not proof of Forge usability. |
| [pbakaus/impeccable](https://github.com/pbakaus/impeccable/tree/9fadc41729c38fd47d7ae622b501558653f9d2e9) · [Apache-2.0](skill-licenses/impeccable-Apache-2.0.txt) · [NOTICE](skill-licenses/impeccable-NOTICE.md) | [Skill](https://github.com/pbakaus/impeccable/blob/9fadc41729c38fd47d7ae622b501558653f9d2e9/.agents/skills/impeccable/SKILL.md), [operate](https://github.com/pbakaus/impeccable/blob/9fadc41729c38fd47d7ae622b501558653f9d2e9/skill/reference/operate.md), [craft floor](https://github.com/pbakaus/impeccable/blob/9fadc41729c38fd47d7ae622b501558653f9d2e9/skill/reference/craft-floor.md) | Task-oriented precision, preservation during polish and rendered-result review. This is a modified textual adaptation, not upstream's skill/runtime. Exclude automatic binary download/context launcher, hooks, pin/doctor mutations, configuration and forced redesign. Native iOS/Android reference bodies are not included or adapted; the upstream notice crediting ehmo is retained unchanged. |
| [emilkowalski/skills](https://github.com/emilkowalski/skills/tree/f1090bec277140d02ed202440185ad9d02ac1490) · [MIT](skill-licenses/emil-skills-MIT.txt) | [Design engineering](https://github.com/emilkowalski/skills/blob/f1090bec277140d02ed202440185ad9d02ac1490/skills/emil-design-eng/SKILL.md), [remove animations](https://github.com/emilkowalski/skills/blob/f1090bec277140d02ed202440185ad9d02ac1490/skills/remove-animations/SKILL.md), [performance](https://github.com/emilkowalski/skills/blob/f1090bec277140d02ed202440185ad9d02ac1490/skills/animation-performance/SKILL.md) | Restraint for repeated/keyboard interactions, interruptibility, reduced motion and measured performance. Express motion through validated tokens/components and generated presentation, not React/Motion/Expo or handwritten CSS/JS recipes. Timing budgets and acceleration claims are not treated as universal guarantees. |
| [microsoft/playwright-mcp](https://github.com/microsoft/playwright-mcp/tree/176c8ea1832d5d0b36963af38191ac0088abdac6) · [Apache-2.0](skill-licenses/playwright-mcp-Apache-2.0.txt) | [README](https://github.com/microsoft/playwright-mcp/blob/176c8ea1832d5d0b36963af38191ac0088abdac6/README.md) | Modified workflow guidance: snapshot-driven targeting plus screenshot review, isolated disposable exploration and explicit evidence scopes. Existing `make ui-qa` stays the acceptance gate. No installation, `@latest` command, real persistent profile, extension attachment, unrestricted filesystem, exposed server or disabled sandbox. Origin filters are not a security boundary. |

License files and Impeccable's notice are retained from those exact revisions.
They cover their respective upstream material and do not relicense unrelated
homelab code. No upstream endorsement or full conformance is claimed. Keep this
modification record and the applicable licenses/notices with any later copied or
adapted material; review new source revisions before changing the pins.

## Evidence boundaries

Adding or invoking these skills does not authorize installations, new services,
new backend capabilities, publication or deployment. Existing project behavior,
test catalog and public APIs must be checked at the current head when used.
The [browser guide](../../src/rust/forge-ide-web/tests/browser/README.md) separates
real-agent acceptance, actual-WASM probes and injected failures. Skill/link
validation is a documentation check, not a fresh browser, accessibility or backend
test certification.
