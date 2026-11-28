# The model-route recovery services in forge-work: a stopped
# binary pin, a provider quota route, and the quota wait/resume.
# Only the captured evidence is authority; every recovery is
# recorded as an audited action. Bindings: tests/bdd_model_route.rs.

Feature: Model route recovery follows the evidence
  A stopped or quota-blocked task resumes only on the evidence
  bound to its captured attempt, and each recovery is recorded
  exactly once.

  Scenario: A stopped binary pin is recovered on verified evidence
    Given the recovery service can judge a stopped attempt
    When the operator presents verified binary evidence
    Then the task is resumed
    And the pin recovery is recorded

  Scenario: A replayed pin recovery returns the original receipt
    Given the recovery service can judge a stopped attempt
    When the same pin recovery is presented again
    Then the original receipt is returned
    And nothing new is recorded

  Scenario: A quota-blocked task routes to its backup model
    Given the recovery service can judge a quota-blocked task
    When the operator routes the task to the backup model
    Then the task is resumed
    And the route to the backup is recorded

  Scenario: A second route is refused
    Given the recovery service can judge a quota-blocked task
    When a route is attempted after an earlier route
    Then the route is refused

  Scenario: A quota wait is recorded when no reset is reported
    Given the recovery service can judge a quota-blocked task
    When the quota wait is checked
    Then the wait is recorded
    And the task stays blocked

  Scenario: The task resumes after the reported reset
    Given the recovery service can judge a quota-blocked task
    When the reported reset has passed
    Then the resume is recorded

  Scenario: Resumes stop at the episode limit
    Given the recovery service can judge a quota-blocked task
    When the episode resume limit is already reached
    Then no resume is recorded
