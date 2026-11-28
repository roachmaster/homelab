# The generated dog scene, described the way people talk about dogs.
# Each sentence is bound to one fixed step in tests/bdd_dog.rs; the answers
# come from generated/dog.rs, never from the step code.

Feature: A dog and the people who look after it
  A dog's owner can be found from its collar or from its chip.
  A dog with neither is treated as a stray.

  Scenario: A stranger reads the collar
    Given a dog can have an owner
    When it wears a collar
    Then a stranger can read its owner's name

  Scenario: The shelter scans the chip
    Given a dog can have an owner
    When it has no collar but is chipped
    Then a shelter scan finds its owner

  Scenario: Nobody can find the owner
    Given a dog can have an owner
    When it has no collar and no chip
    Then it is treated as a stray

  Scenario: Calm in the crate
    Given a dog can be crate trained
    When it is crate trained
    Then it is calm in a crate

  Scenario: Whining in the crate
    Given a dog can be crate trained
    When it is not crate trained
    Then it whines in a crate

  Scenario: Engraved collars
    Given a collar can be engraved with any name
    When collars are engraved for "Ana" and "Bo"
    Then each collar shows its own name

  Scenario: One dog in the household
    Given a household can keep one dog
    When the family greets the dog twice
    Then they greet the same dog both times

  Scenario: Every training session starts fresh
    Given a dog can be trained in sessions
    When it has two training sessions
    Then each session starts fresh

  Scenario: The chip registry is looked up only when someone scans
    Given a chip registry can wait until someone scans
    When nobody has scanned the chip yet
    Then the registry has not been looked up
    And the first scan looks it up once

  Scenario: A kennel with no dog
    Given a kennel needs a dog
    When there is no dog to board
    Then the kennel will not open
    And it says a dog is missing

  Scenario: The pet sitter keeps count
    Given a dog may be watched by a sitter
    When the sitter is watching and the dog meets one stranger
    Then the sitter has seen one visit

  Scenario: A pretend dog for tests
    Given any dog can be swapped for a pretend one
    When we are testing
    Then the pretend dog answers
    And the collar still shows "Ana"
    And the kennel still boards whoever the dog is
