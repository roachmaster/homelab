"""One bounded metadata operation between jobs; no generation or registry lookup."""

from collections import deque
from copy import deepcopy
from dataclasses import dataclass
import time
import os
import re
import stat
from types import MappingProxyType

from usage_rpc import ProviderFailure
from usage_types import InvalidUsage, MAX_PROVIDERS, UUID, exact, identifier, text, validate_snapshot


@dataclass(frozen=True)
class _Poll:
    profile_alias: str
    generation: int
    configuration_epoch: int


@dataclass
class _Pending:
    poll: _Poll
    snapshot: dict | None
    expires_at: float


def _poll(profile, value):
    exact(value, {"generation", "configuration_epoch"})
    generation = value["generation"]
    if type(generation) is not int or not 1 <= generation <= 2**53 - 1:
        raise InvalidUsage("invalid metadata poll generation")
    epoch = value["configuration_epoch"]
    if type(epoch) is not int or not 1 <= epoch <= 2**53 - 1:
        raise InvalidUsage("invalid metadata configuration epoch")
    return _Poll(identifier(profile), generation, epoch)


class UsageCollector:
    """Own ready provider ports and a scoped store. Every call is bounded.

    Failed database publication retains one sanitized snapshot for retry without
    making another provider request. The durable refresh request itself belongs
    to the store. A worker restart can safely re-poll metadata, never coding work.
    """

    def __init__(self, providers, store, *, poll_interval=60, outbox_ttl=300, monotonic=time.monotonic):
        selected = dict(providers)
        if not selected or len(selected) > MAX_PROVIDERS or store is None:
            raise ValueError("Usage collector requires selected providers and store")
        for alias, provider in selected.items():
            if identifier(alias) != provider.config.alias or not callable(provider.read_snapshot):
                raise ValueError("Usage provider identity mismatch")
        if (type(poll_interval) not in (int, float) or not 5 <= poll_interval <= 3600
                or type(outbox_ttl) not in (int, float) or not 5 <= outbox_ttl <= 3600):
            raise ValueError("Invalid usage collector timing")
        self._providers = MappingProxyType(selected)
        self._store = store
        self._interval = poll_interval
        self._ttl = outbox_ttl
        self._clock = monotonic
        self._next_due = {alias: 0 for alias in selected}
        self._order = tuple(selected)
        self._index = 0
        self._pending = deque(maxlen=1)

    def _failed(self, poll, code):
        try:
            self._store.fail_usage_poll(profile_alias=poll.profile_alias, generation=poll.generation,
                                        configuration_epoch=poll.configuration_epoch, code=code)
        except Exception:
            # A telemetry outage must not fail completed coding work or spin.
            return False
        return True

    def _publish_pending(self):
        pending = self._pending[0]
        if self._clock() >= pending.expires_at:
            pending.snapshot = None
        if pending.snapshot is None:
            if not self._failed(pending.poll, "unavailable"):
                return False
        else:
            try:
                # False means an old generation/epoch; it must be discarded, not
                # repeatedly retried or rewritten using a new generation.
                self._store.record_usage(profile_alias=pending.poll.profile_alias,
                                         generation=pending.poll.generation,
                                         configuration_epoch=pending.poll.configuration_epoch,
                                         snapshot=deepcopy(pending.snapshot))
            except Exception:
                return False
        self._pending.popleft()
        return True

    def run_once(self):
        if self._pending:
            return self._publish_pending()
        try:
            refresh = self._store.next_refresh()
        except Exception:
            return False
        if refresh is not None:
            try:
                exact(refresh, {"refresh_id", "profile_alias", "generation", "configuration_epoch"})
                if not isinstance(refresh["refresh_id"], str) or not UUID.fullmatch(refresh["refresh_id"]):
                    raise InvalidUsage("invalid metadata refresh identity")
                alias = identifier(refresh["profile_alias"])
                poll = _poll(alias, {"generation": refresh["generation"],
                                     "configuration_epoch": refresh["configuration_epoch"]})
            except InvalidUsage:
                return False
        else:
            alias = None
            now = self._clock()
            for offset in range(len(self._order)):
                index = (self._index + offset) % len(self._order)
                candidate = self._order[index]
                if (getattr(self._providers[candidate], "manual_refresh_only", False) is not True
                        and now >= self._next_due[candidate]):
                    alias = candidate
                    self._index = (index + 1) % len(self._order)
                    break
            if alias is None:
                return False
            try:
                poll = _poll(alias, self._store.begin_usage_poll(profile_alias=alias))
            except Exception:
                return False
        self._next_due[alias] = self._clock() + self._interval
        provider = self._providers.get(alias)
        if provider is None:
            return self._failed(poll, "unknown_profile")
        try:
            snapshot = deepcopy(validate_snapshot(provider.read_snapshot()))
        except ProviderFailure as error:
            return self._failed(poll, error.code)
        except InvalidUsage:
            return self._failed(poll, "malformed_metadata")
        except Exception:
            return self._failed(poll, "unavailable")
        self._pending.append(_Pending(poll, snapshot, self._clock() + self._ttl))
        return self._publish_pending()


class UsageCollectorFactory:
    """Effect-free selection; construction/acquisition happens only in create."""

    def __init__(self, provider_factories, store=None, *, poll_interval=60, outbox_ttl=300,
                 monotonic=time.monotonic):
        if provider_factories is not None:
            provider_factories = tuple(provider_factories)
            if not provider_factories or len(provider_factories) > MAX_PROVIDERS or store is None:
                raise ValueError("Usage collector requires provider factories and store")
            if any(not callable(getattr(factory, "create", None)) for factory in provider_factories):
                raise TypeError("Usage collector requires selected factories")
            aliases = [identifier(factory.config.alias) for factory in provider_factories]
            if len(aliases) != len(set(aliases)):
                raise ValueError("Duplicate usage provider alias")
        if (type(poll_interval) not in (int, float) or not 5 <= poll_interval <= 3600
                or type(outbox_ttl) not in (int, float) or not 5 <= outbox_ttl <= 3600):
            raise ValueError("Invalid usage collector timing")
        self._factories = provider_factories
        self._store = store
        self._interval = poll_interval
        self._ttl = outbox_ttl
        self._clock = monotonic

    def create(self):
        if self._factories is None:
            return None
        providers, acquired = {}, []
        try:
            for factory in self._factories:
                provider = factory.create()
                acquired.append(provider)
                if provider.config.alias != factory.config.alias:
                    raise ValueError("Selected usage provider identity changed")
                providers[provider.config.alias] = provider
            return UsageCollector(providers, self._store, poll_interval=self._interval,
                                  outbox_ttl=self._ttl, monotonic=self._clock)
        except Exception:
            for provider in reversed(acquired):
                close = getattr(provider, "close", None)
                if callable(close):
                    close()
            raise


def _read_hmac_key(path):
    """Acquire only this root-configured control secret; never provider credentials."""
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            info = os.fstat(stream.fileno())
            if (not stat.S_ISREG(info.st_mode) or not 1 <= info.st_size <= 65
                    or info.st_mode & 0o077 or info.st_uid not in (0, os.geteuid())):
                raise ValueError("Usage HMAC key must be private to its worker")
            value = stream.read(66)
        if len(value) == 32:
            return value
        if re.fullmatch(b"[0-9a-fA-F]{64}\n?", value):
            return bytes.fromhex(value.strip().decode("ascii"))
    except OSError:
        raise ValueError("Unable to read usage HMAC key") from None
    raise ValueError("Usage HMAC key must contain32bytes or64hex digits")


def create_worker_usage_collector(config, store):
    """Composition adapter called by CodingWorkerFactory only after validation."""
    if config.usage is None:
        return None
    from usage_provider import UsageProviderConfig, UsageProviderFactory
    from usage_rpc import MetadataProcessConfig

    key = _read_hmac_key(config.usage.hmac_key_file)
    account = config.account
    if os.geteuid() not in (0, account.uid):
        raise ValueError("Usage collection requires the configured signed-in account")
    factories = []
    for profile in config.profiles.values():
        process = None
        if profile.backend in {"codex", "muse"}:
            process = MetadataProcessConfig(
                binary=config.binaries[profile.backend], home=account.home,
                code_home=account.home / ".codex" if profile.backend == "codex" else None,
                run_uid=account.uid if os.geteuid() == 0 else None,
                run_gid=account.gid if os.geteuid() == 0 else None,
            )
        selected = UsageProviderConfig(profile.alias, profile.alias, profile.backend, profile.model, process)
        factories.append(UsageProviderFactory(selected, hmac_key=key))
    return UsageCollectorFactory(factories, store, poll_interval=config.usage.interval_seconds).create()
