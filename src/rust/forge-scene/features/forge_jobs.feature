# forge-jobs is a plug-and-play module: a store group and a worker group that
# needs the store. Bindings: tests/bdd_forge_jobs.rs.

Feature: Durable jobs plug into any host
  The job store and its worker are separate factory groups.
  The host chooses how each is provided; the worker always needs a store.

  Scenario: The front desk and the worker share one store
    Given the job store can be shared by everyone in the host
    When a job is submitted at the front desk
    Then the worker can claim that same job

  Scenario: The store stays closed until the first job
    Given the job store can wait until the first job arrives
    When no job has been submitted
    Then the store directory is still untouched

  Scenario: The first job opens the store
    Given the job store can wait until the first job arrives
    When the first job is submitted
    Then the store directory holds the job lock
    And the job is waiting in the queue

  Scenario: Each request for a worker starts a new one
    Given workers can be started whenever the host asks
    When the host asks for two workers
    Then two separate workers are running
    And both stop cleanly

  Scenario: A worker with no store
    Given a worker needs a job store
    When the execution group is staged without a store
    Then startup refuses to freeze
    And the refusal names the job store

  Scenario: Tests swap only the store
    Given the job store can be swapped for a test double
    When the test host wires the in-memory store
    Then a submitted job lands in memory
    And the front desk still hands back a receipt
