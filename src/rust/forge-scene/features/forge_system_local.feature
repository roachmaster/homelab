# Disk capability registration (forge-system) and local host command
# dispatch (forge-local): registration validates paths without touching
# the filesystem, optional disk capability stays absent when unconfigured,
# and the local bridge enforces header/payload-variant agreement.
# Bindings: tests/bdd_forge_system_local.rs.

Feature: Disk registration and local host bridge enforce typed boundaries
  forge-system::disk validates startup configuration without filesystem
  I/O and defers directory checks to factory creation; forge-local binds
  disk routes only when enabled and rejects mismatched command envelopes.

  Scenario: A relative disk directory is rejected during registration
    Given the disk registrar needs an explicit absolute starting directory
    When registration receives a relative path
    Then registration fails with an absolute-path configuration error

  Scenario: A missing absolute directory fails at creation rather than registration
    Given the disk registrar can stage configuration without filesystem access
    When a nonexistent absolute directory is registered and frozen
    Then staging succeeds and factory creation fails on the missing directory

  Scenario: Bootstrapping without a disk directory omits disk commands
    Given the local host may start without the disk capability
    When the application bootstraps with no disk directory
    Then command discovery omits disk.list

  Scenario: Bootstrapping with a valid directory publishes all disk commands
    Given the local host can enable the disk capability from a directory
    When the application bootstraps with a temporary directory
    Then command discovery includes disk.pwd, disk.list, disk.cd, and disk.permissions

  Scenario: An admitted disk.list returns entries in deterministic order
    Given the local host can list files in its configured directory
    When disk.list is invoked on a directory containing beta.txt and alpha.txt
    Then the listing returns alpha.txt before beta.txt

  Scenario: A header and payload mismatch is rejected by the local bridge
    Given the local host can verify disk command variants against route headers
    When route disk.list receives a pwd command payload
    Then invocation fails with a payload mismatch error
