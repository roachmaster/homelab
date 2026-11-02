// Once the fixture leader has unexpectedly exited, its old process-group ID is
// not safe to signal: it may be gone/reused, or an agent descendant may remain.
// Report cleanup as failed/unconfirmed instead of claiming those descendants
// were stopped. This is a policy guard, not a descendant process supervisor.
export function requireRunningFixtureLeader(child) {
  if (child.pid && (child.exitCode !== null || child.signalCode !== null)) {
    throw new Error('Unexpected fixture leader exit leaves cleanup unconfirmed');
  }
}
