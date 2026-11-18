# Forge plug-and-play

Start with [Wire Forge Providers](.agents/skills/wire-forge-providers/SKILL.md)
before designing, implementing, or reviewing a Forge capability, module,
service, adapter, worker, or runtime dependency. Use specialist skills from
that entry point when the task calls for them.

In the normal application path, bootstrap selects the implementation and
scope through typed configuration. Stage effect-free registrars, validate and
freeze the startup registry, create the selected instances, and inject ready
ports. Inject a scoped provider where the consumer needs runtime acquisition.
Keep dependency selection at the composition root and keep pure logic and
contract types independent of provisioning.

Before changing a page, layout, or frontend component, use
[Design Modern UI](.agents/skills/design-modern-ui/SKILL.md) to define its
visual hierarchy and interaction states. Follow
[Design Forge UI](.agents/skills/design-forge-ui/SKILL.md) for typed Rust/YAML
authoring and [Test Forge UI](.agents/skills/test-forge-ui/SKILL.md) to verify
the rendered result.
