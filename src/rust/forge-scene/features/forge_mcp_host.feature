# The forge-mcp-host server advertises its MCP surfaces, dispatches
# typed MissionControlPort requests over the fake backend with zero
# external effects, and publishes strict tool argument schemas.
# Bindings: tests/bdd_forge_mcp_host.rs.

Feature: MCP host advertises surfaces, dispatches requests, and publishes schemas
  McpServer wraps a MissionControlPort backend so fake-backend tests
  exercise server metadata, request dispatch, event capping, and tool
  input schemas without touching live infrastructure.

  Scenario: Server info names the service and advertises all surfaces
    Given the MCP server can report its capabilities
    When server info is queried
    Then the service name is forge-mcp with tools, resources, prompts, and instructions enabled

  Scenario: Work get returns the fake task view
    Given the MCP server can dispatch requests over the fake backend
    When a work get request asks for task-9
    Then the reply returns task-9 awaiting review on muse-db-primary

  Scenario: Workflow get returns an enabled implement workflow
    Given the MCP server can dispatch requests over the fake backend
    When a workflow get request asks for wf-9
    Then the reply returns wf-9 enabled in the implement phase

  Scenario: Events list caps at five newest first and honors smaller limits
    Given the MCP server can dispatch requests over the fake backend
    When events are requested with limit ten and then limit three
    Then the first reply has five descending sequences and the second has three

  Scenario: Control mutations return a fresh UUID on submit and echo task IDs
    Given the MCP server can dispatch requests over the fake backend
    When submit, cancel, and decide requests are executed
    Then submit yields a valid UUID while cancel and decide echo task-9

  Scenario: Input schemas name required arguments and stay open for unknown tools
    Given the MCP host can describe tool input schemas
    When schemas are queried for work_get, work_submit, and an unknown tool
    Then known tools list their required fields and the unknown tool returns an unconstrained object
