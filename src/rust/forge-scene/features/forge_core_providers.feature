# forge-core's provider and startup-registry contracts, in their own words.
# Bindings: tests/bdd_forge_core.rs.

Feature: Providers hand out values and the registry checks startup
  forge-core keeps construction, acquisition and startup validation apart.
  A factory builds, a provider decides who shares, and the registry refuses a
  plan that cannot start.

  Scenario: A singleton shares its value
    Given a provider can hold one prebuilt value
    When two consumers acquire a lease
    Then both consumers see the same value
    And the factory has built one value

  Scenario: A transient builds for every lease
    Given a provider can build a value for every lease
    When two consumers acquire a lease
    Then each consumer owns its own value
    And the factory has built two values

  Scenario: A lazy provider waits for its first lease
    Given a provider can wait to build its value
    When nobody has acquired a lease yet
    Then the factory has not run

  Scenario: The first lease builds the lazy value once
    Given a provider can wait to build its value
    When two consumers acquire a lease
    Then both consumers see the same value
    And the factory has built one value

  Scenario: Startup refuses a missing binding
    Given a registry can stage factory groups
    When a group needs "clock" and nobody provides it
    Then the registry refuses to freeze
    And the refusal names "clock"

  Scenario: Startup refuses a group staged too early
    Given a registry can stage factory groups
    When a group needing "clock" is staged before the group providing it
    Then the registry refuses to freeze
    And the refusal says "clock" must be provided first

  Scenario: A ready registry hands back its factories
    Given a registry can stage factory groups
    When "clock" is staged before the group that needs it
    Then the frozen registry builds both values in order
