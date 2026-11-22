"""Dormant external Muse intake; no worker, publication or process authority."""
from work_muse_intake import ExternalIntakeUnsupported, MuseIntakeConfig, UNSUPPORTED


def apply_publish_result(worker_db, config, incarnation, row):
    """External data cannot mint a native attempt or reconciliation receipt."""
    raise ExternalIntakeUnsupported(UNSUPPORTED)


class MuseIntakeConsumer:
    """Construction has no effects; no consumer loop is provisioned."""
    def __init__(self, config, worker_db, intake_store, *, stop=None):
        self.config, self.worker_db, self.intake = config, worker_db, intake_store
        self.stop = stop

    def advertise(self):
        raise ExternalIntakeUnsupported(UNSUPPORTED)

    def run_once(self):
        raise ExternalIntakeUnsupported(UNSUPPORTED)

    def run_forever(self):
        raise ExternalIntakeUnsupported(UNSUPPORTED)


class MuseIntakeConsumerFactory:
    """Stage explicit ports without acquiring them; activation is unsupported."""
    def __init__(self, config, worker_factory, intake_factory):
        if not isinstance(config, MuseIntakeConfig):
            raise TypeError("Expected frozen Muse intake configuration")
        if config.enabled:
            raise ExternalIntakeUnsupported(UNSUPPORTED)
        if not callable(worker_factory) or not callable(intake_factory):
            raise TypeError("Expected explicit intake port factories")
        self.config = config
        self.worker_factory, self.intake_factory = worker_factory, intake_factory

    def create(self):
        raise ExternalIntakeUnsupported(UNSUPPORTED)


def default_store_factory(config):
    raise ExternalIntakeUnsupported(UNSUPPORTED)


def default_intake_factory(config):
    raise ExternalIntakeUnsupported(UNSUPPORTED)


def main():
    # Reject before opening configuration, credentials, database or process ports.
    raise SystemExit(UNSUPPORTED)


if __name__ == "__main__":
    main()
