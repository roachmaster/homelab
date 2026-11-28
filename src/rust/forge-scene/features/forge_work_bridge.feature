# The forge-work ingress bridge enforces admission and header/variant
# agreement before any port runs: closed admission, mismatched payload
# variants, unknown destinations, and unsupported versions produce
# zero port calls. Bindings: tests/bdd_forge_work_bridge.rs.

Feature: Work bridge enforces admission and typed header agreement
  Inbound work operations cross a frozen fifteen-route bridge that
  checks readiness and payload-kind agreement before invoking any
  read, admission, or decision port.

  Scenario: Open admission completes a valid list request
    Given the work bridge can dispatch registered operations
    When an admitted list envelope arrives with a correlation token
    Then the response carries the matching correlation header and task page

  Scenario: Closed admission rejects work without touching ports
    Given the work bridge can deny traffic when admission is closed
    When a list envelope arrives while admission is closed
    Then dispatch is denied and no port is called

  Scenario: A header and payload mismatch is rejected before effects
    Given the work bridge can verify payload kinds against headers
    When a get header carries a list payload
    Then dispatch rejects the mismatch and no port is called

  Scenario: An unknown destination is rejected before effects
    Given the work bridge can reject unregistered destinations
    When an envelope targets an unregistered destination
    Then dispatch reports an unknown route and no port is called

  Scenario: An unsupported contract version is rejected before effects
    Given the work bridge can reject unsupported contract versions
    When a list envelope requests an unsupported version
    Then dispatch reports an unknown route and no port is called

  Scenario: A port error propagates to the caller
    Given the work bridge can surface failures from its read port
    When an admitted get request hits a failing read port
    Then dispatch returns the handler error after one port call
