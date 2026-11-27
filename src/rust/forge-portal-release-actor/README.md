# forge-portal-release-actor

Fixed-scope Root portal release actor (S7 port of
deploy/mission-control/portal_release_actor.py): the configured
caller selects one already-verified release identity; Root chooses
artifacts, staging paths, the service, health probes and the
rollback target. The bin reads one JSON request on stdin under the
activation lock and prints one canonical outcome line.
