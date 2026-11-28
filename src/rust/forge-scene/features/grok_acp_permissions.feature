# The Grok ACP transport separates progress from permission authority:
# only the captured call is evidence, and a permission request must
# prove it restates that call. Bindings: tests/bdd_grok_acp.rs.

Feature: ACP permission follows the captured call
  Progress annotations are display data. The arguments captured when
  a tool call is bound are the only authority a permission request
  can be judged against.

  Scenario: A complete descriptor proves the captured command
    Given the permission broker can judge a captured tool call
    When a terminal call is captured with its arguments
    And a permission request restates them in a complete descriptor
    Then the request is allowed once

  Scenario: A descriptor naming another command proves nothing
    Given the permission broker can judge a captured tool call
    When a terminal call is captured with its arguments
    And a permission request describes a different command
    Then the request is rejected once

  Scenario: Changed arguments at permission time are rejected
    Given the permission broker can judge a captured tool call
    When a terminal call is captured with its arguments
    And a permission request presents changed arguments
    Then the request is rejected once

  Scenario: Progress cannot rewrite the captured arguments
    Given the permission broker can judge a captured tool call
    When a terminal call is captured with its arguments
    And a progress update reports different arguments
    Then the update is accepted silently
    And the captured call can still be judged

  Scenario: A call with malformed native metadata is refused at capture
    Given the permission broker can judge a captured tool call
    When a tool call arrives with incomplete native metadata
    Then the update is refused

  Scenario: A pathless grep reads the workspace
    Given the broker can admit read-only searches
    When a grep names no path and stays within its bounds
    Then the grep is admitted

  Scenario: A grep beyond its bounds is refused
    Given the broker can admit read-only searches
    When a grep asks for more context than the bound allows
    Then the grep is refused
