# SystemdSupervisor stop proofs over ProcessCommandPort + process_exec
# IoClient: invalid unit names are rejected before any command runs,
# stop proofs require inactive/failed state with MainPID=0 and an empty
# matching cgroup, and stop() always follows stop with proof().
# Bindings: tests/bdd_worker_supervisor.rs.

Feature: Worker supervisor verifies systemd scope stop proofs strictly
  SystemdSupervisor inspects unit state through ProcessCommandPort and
  the cgroup tree. A scope is considered stopped only when systemd
  reports inactive or failed, MainPID is zero, the ControlGroup matches,
  and the cgroup is absent or unpopulated.

  Scenario: An invalid scope name is rejected without running commands
    Given the supervisor can validate coding scope names
    When a stop proof is requested for an invalid scope name
    Then proof fails with a protocol error and zero commands run

  Scenario: An inactive unit with MainPID zero and an unpopulated cgroup proves stopped
    Given the supervisor can inspect systemd state over an injected executor
    When systemctl reports inactive with MainPID zero and an unpopulated cgroup
    Then the stop proof confirms the scope is stopped

  Scenario: A populated cgroup prevents a stop proof
    Given the supervisor can inspect systemd state over an injected executor
    When systemctl reports inactive with MainPID zero while cgroup events remain populated
    Then the stop proof reports the scope is still running

  Scenario: An active unit with a live MainPID prevents a stop proof
    Given the supervisor can inspect systemd state over an injected executor
    When systemctl reports active with a nonzero MainPID
    Then the stop proof reports the scope is still running

  Scenario: A mismatched ControlGroup prevents a stop proof
    Given the supervisor can inspect systemd state over an injected executor
    When systemctl reports a different ControlGroup path
    Then the stop proof reports the scope is still running

  Scenario: Stopping a unit issues systemctl stop and verifies with show
    Given the supervisor can stop a coding scope over an injected executor
    When stop is requested for a valid coding scope
    Then systemctl stop runs before systemctl show and confirms the scope is stopped
