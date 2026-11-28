# Publication runtime registration and process-backed ports:
# register_publication requires the worker's process_exec binding at
# freeze. Observer execution takes an injected IoClient, and the
# historical policy parser reads source directly in Rust.
# Bindings: tests/bdd_forge_work_publication.rs.

Feature: Publication registrar and process-backed ports honor contracts
  The publication runtime stages its observer and AST factories in a
  PreparedGroup, enforces its process_exec requirement at freeze, and
  surfaces bounded observer and parser outcomes cleanly.

  Scenario: Freezing publication without process execution fails
    Given the publication runtime needs the worker process executor
    When publication is staged alone and frozen
    Then freeze fails because the required binding is missing

  Scenario: Staging after process execution freezes and refuses duplicates
    Given the publication runtime can register alongside the worker process executor
    When both groups are staged and publication is submitted a second time
    Then the duplicate is refused and the frozen factories construct cleanly

  Scenario: Observer execution returns stdout on zero exit
    Given the publication observer can run over an injected executor
    When the observer runs a command that exits with zero status
    Then the observer returns the captured stdout payload

  Scenario: Observer execution translates a time-bound failure
    Given the publication observer can run over an injected executor
    When the executor reports a time-bound protocol error
    Then the observer reports a timed-out command

  Scenario: Observer execution rejects an empty argument list
    Given the publication observer can run over an injected executor
    When the observer is given an empty argument list
    Then the observer refuses the command with zero executor calls

  Scenario: Verifier AST parsing preserves structure and reports invalid syntax
    Given the historical verifier factory can parse policy source
    When the historical policy parser reads valid source and then encounters invalid syntax
    Then the first call yields the parsed module and the second returns the parser error
