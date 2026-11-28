# forge-template's renderer registrar as a plug-and-play module.
# Bindings: tests/bdd_forge_template.rs.

Feature: Templates are registered at startup and rendered on demand
  A renderer is staged with its schema and methods, frozen, then provided.
  The host decides whether pages share one renderer or parse their own.

  Scenario: One renderer serves every page
    Given a renderer can be shared by every page
    When two pages are rendered
    Then both pages come from the same renderer
    And both pages read "Hello, FORGE!"

  Scenario: A broken template is found when it is first used
    Given a template can be registered before it is parsed
    When its source is broken
    Then startup still freezes
    And the first page request fails

  Scenario: Builtin methods are there unless the host adds its own
    Given a renderer may have custom methods
    When the host configures none
    Then the builtin methods still work

  Scenario: Two templates cannot share a name
    Given a host can register many templates
    When two templates claim the name "greeting"
    Then the second one is refused
    And the first one keeps its place
