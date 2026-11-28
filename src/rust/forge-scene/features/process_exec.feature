# Bounded subprocess execution behind forge-core's IoClient:
# requests carry explicit argv, timeout and environment policy;
# registration stages a factory without spawning. Bindings:
# tests/bdd_process_exec.rs.

Feature: Worker process execution honors typed bounds and registration
  Bounded subprocess calls cross a typed IoClient boundary with explicit
  environment isolation, wall-clock deadlines, and effect-free bootstrap
  registration.

  Scenario: An empty command is refused before spawning
    Given the process executor can run bounded commands
    When an empty argument list is submitted
    Then execution fails with a protocol error

  Scenario: Standard input is echoed back cleanly
    Given the process executor can run bounded commands
    When a command reads its provided standard input
    Then the exit status is zero and stdout matches the input

  Scenario: A cleared environment strips ambient variables
    Given the process executor can run bounded commands
    When a command runs with a cleared environment and one explicit marker
    Then stdout contains the marker and omits ambient home variables

  Scenario: A slow command is killed at its deadline
    Given the process executor can run bounded commands
    When a command sleeps past a one-second deadline
    Then execution fails with a time-bound error

  Scenario: Registration stages once and rejects duplicates
    Given the process executor can be registered at bootstrap
    When the group is registered twice before freeze
    Then the duplicate is rejected and the frozen factory produces a working client

  Scenario: Git runner masks failures behind a hardened request
    Given a git runner needs a process executor
    When the underlying process exits with a nonzero status
    Then the runner reports the repository as unavailable
