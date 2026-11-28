import assert from 'node:assert/strict';
import test from 'node:test';
import { journeyFailureLocation } from './diagnostics.mjs';

test('nested known journey frame wins and discloses only module and coordinates', () => {
  const stack = 'Error: private assertion prose must not escape\n'
    + '    at exerciseAgentsTriage (file:///private/credential-directory/agents-triage.mjs:259:23)\n'
    + '    at async agentsTriageJourney (file:///private/journey.mjs:3893:5)';
  assert.equal(journeyFailureLocation({stack}), 'agents-triage.mjs:259:23');
  assert.equal(journeyFailureLocation({stack:'Error: private\n    at /private/journey.mjs:42:9'}), 'journey.mjs:42:9');
  assert.equal(journeyFailureLocation({stack:'Error: '+ 'private'.repeat(10000) + '\n    at /private/agents-triage.mjs:259:23'}), 'agents-triage.mjs:259:23');
});

test('unknown modules, prose, remote URLs and malformed or excessive coordinates stay private', () => {
  for (const stack of [
    'Error: agents-triage.mjs:259:23',
    'Error\n    private text agents-triage.mjs:259:23',
    'Error\n    at source (file:///private/unknown.mjs:259:23)',
    'Error\n    at source (https://private.example/agents-triage.mjs:259:23)',
    'Error\n    at source (file:///private/agents-triage.mjs:0:23)',
    'Error\n    at source (file:///private/agents-triage.mjs:-1:23)',
    'Error\n    at source (file:///private/agents-triage.mjs:1000000:23)',
    'Error\n    at source (file:///private/agents-triage.mjs:259:10000)',
    'Error\n    at source (file:///private/agents-triage.mjs:259:23?secret=private)',
    'Error: '+ 'x'.repeat(16384),
  ]) assert.equal(journeyFailureLocation({stack}), null);
  assert.equal(journeyFailureLocation(null), null);
  assert.equal(journeyFailureLocation({stack:[]}), null);
});
