# Provider quota cooldowns withhold exhausted profiles locally until
# their retry window expires, fail closed on unsafe or missing state,
# and ignore malformed diagnostics. Bindings: tests/bdd_worker_cooldown.rs.

Feature: Worker cooldown withholds exhausted profiles safely
  ProviderCooldown tracks stopped CLI quota diagnostics in a private
  state directory. It can only withhold admission; it never fabricates
  eligibility when state is unsafe or missing.

  Scenario: Fresh state admits all configured profiles
    Given the worker cooldown can evaluate configured profiles
    When no quota diagnostics have been recorded
    Then both configured profiles remain admitted

  Scenario: A quota diagnostic withholds only the exhausted profile
    Given the worker cooldown can evaluate configured profiles
    When a quota exhaustion diagnostic is recorded for the primary profile
    Then only the backup profile remains admitted

  Scenario: Admission resumes once the retry window expires
    Given the worker cooldown can evaluate configured profiles
    When a quota exhaustion diagnostic is recorded for the primary profile
    And the clock advances past the retry window
    Then both configured profiles remain admitted

  Scenario: Replaying the same attempt diagnostic is idempotent
    Given the worker cooldown can evaluate configured profiles
    When the same attempt diagnostic is recorded twice after the clock moves slightly
    And the clock advances past the original retry window
    Then both configured profiles remain admitted

  Scenario: A malformed diagnostic is ignored safely
    Given the worker cooldown can evaluate configured profiles
    When a malformed quota diagnostic is recorded
    Then both configured profiles remain admitted

  Scenario: A missing state root fails closed
    Given the worker cooldown needs a valid state directory
    When selection runs against a missing state parent
    Then every profile is withheld
