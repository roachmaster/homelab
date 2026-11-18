use crate::{check_token, Claim, JobError, JobId, JobStore, Owner, Receipt, State, Status, Work};
use forge_bootstrap::PreparedGroup;
use forge_provider::Factory;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const STORE_VERSION: u8 = 1;
const SNAPSHOT_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// An existing private directory owned by the operator.
    pub directory: PathBuf,
    pub max_jobs: usize,
}

impl StoreConfig {
    fn validate(&self) -> Result<(), JobError> {
        if self.directory.as_os_str().is_empty() || !(1..=4096).contains(&self.max_jobs) {
            return Err(JobError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StoreFactory(StoreConfig);

impl Factory<FileStore> for StoreFactory {
    type Error = JobError;

    fn create(&self) -> Result<FileStore, JobError> {
        FileStore::open(self.0.clone())
    }
}

pub fn register_store(config: StoreConfig) -> Result<PreparedGroup<StoreFactory>, JobError> {
    config.validate()?;
    Ok(PreparedGroup::new(
        "durable.jobs",
        &["durable.jobs.store"],
        &[],
        StoreFactory(config),
    )
    .expect("fixed job store group"))
}

#[derive(Debug)]
pub struct FileStore {
    directory: PathBuf,
    max_jobs: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    version: u8,
    jobs: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    owner: Owner,
    key: String,
    work: Work,
    status: Status,
    lease_until: Option<u64>,
}

impl FileStore {
    pub fn open(config: StoreConfig) -> Result<Self, JobError> {
        config.validate()?;
        let directory = config
            .directory
            .canonicalize()
            .map_err(|_| JobError::StoreUnavailable)?;
        if !directory.is_dir() {
            return Err(JobError::StoreUnavailable);
        }
        let store = Self {
            directory,
            max_jobs: config.max_jobs,
        };
        store.access(|_| Ok(((), false)))?;
        Ok(store)
    }

    fn access<T>(
        &self,
        apply: impl FnOnce(&mut Data) -> Result<(T, bool), JobError>,
    ) -> Result<T, JobError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("jobs.lock"))
            .map_err(|_| JobError::StoreUnavailable)?;
        lock.lock().map_err(|_| JobError::StoreUnavailable)?;
        let snapshot = self.directory.join("jobs.json");
        let mut data = load(&snapshot)?;
        let (result, changed) = apply(&mut data)?;
        if changed {
            self.publish(&snapshot, &data)?;
        }
        Ok(result)
    }

    fn publish(&self, snapshot: &Path, data: &Data) -> Result<(), JobError> {
        let encoded = serde_json::to_vec(data).map_err(|_| JobError::StoreUnavailable)?;
        if encoded.len() as u64 > SNAPSHOT_LIMIT {
            return Err(JobError::Capacity);
        }
        let pending = self.directory.join("jobs.pending");
        if let Err(error) = fs::remove_file(&pending) {
            if error.kind() != io::ErrorKind::NotFound {
                return Err(JobError::StoreUnavailable);
            }
        }
        let commit = || -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pending)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&pending, snapshot)?;
            File::open(&self.directory)?.sync_all()
        };
        commit().map_err(|_| JobError::StoreUnavailable)
    }
}

fn load(path: &Path) -> Result<Data, JobError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Data {
                version: STORE_VERSION,
                jobs: Vec::new(),
            });
        }
        Err(_) => return Err(JobError::StoreUnavailable),
    };
    let mut bytes = Vec::new();
    file.take(SNAPSHOT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| JobError::StoreUnavailable)?;
    if bytes.len() as u64 > SNAPSHOT_LIMIT {
        return Err(JobError::StoreUnavailable);
    }
    let data: Data = serde_json::from_slice(&bytes).map_err(|_| JobError::StoreUnavailable)?;
    if data.version != STORE_VERSION
        || data.jobs.len() > 4096
        || data.jobs.iter().enumerate().any(|(index, record)| {
            record.status.id.0 != index as u64 + 1
                || (record.lease_until.is_some() != (record.status.state == State::Running))
                || check_token(&record.owner.0).is_err()
                || check_token(&record.key).is_err()
                || record.work.validate().is_err()
        })
    {
        return Err(JobError::StoreUnavailable);
    }
    Ok(data)
}

fn index(id: JobId) -> Option<usize> {
    usize::try_from(id.0.checked_sub(1)?).ok()
}

fn record<'a>(data: &'a mut Data, owner: &Owner, id: JobId) -> Result<&'a mut Record, JobError> {
    index(id)
        .and_then(|index| data.jobs.get_mut(index))
        .filter(|record| &record.owner == owner)
        .ok_or(JobError::NotFound)
}

fn current(data: &mut Data, claim: Claim, now: u64) -> Result<&mut Record, JobError> {
    index(claim.id)
        .and_then(|index| data.jobs.get_mut(index))
        .filter(|record| {
            record.status.attempt == claim.attempt
                && record.work == claim.work
                && record.lease_until.is_some_and(|deadline| now < deadline)
        })
        .ok_or(JobError::StaleAttempt)
}

fn deadline(now: u64, lease: u64) -> Result<u64, JobError> {
    now.checked_add(lease)
        .filter(|_| lease != 0)
        .ok_or(JobError::InvalidConfiguration)
}

impl JobStore for FileStore {
    fn submit(&self, owner: &Owner, key: &str, work: Work) -> Result<Receipt, JobError> {
        check_token(&owner.0)?;
        check_token(key)?;
        work.validate()?;
        self.access(|data| {
            if let Some(record) = data
                .jobs
                .iter()
                .find(|record| &record.owner == owner && record.key == key)
            {
                if record.work != work {
                    return Err(JobError::Conflict);
                }
                return Ok((
                    Receipt {
                        id: record.status.id,
                    },
                    false,
                ));
            }
            if data.jobs.len() >= self.max_jobs {
                return Err(JobError::Capacity);
            }
            let id = JobId(data.jobs.len() as u64 + 1);
            data.jobs.push(Record {
                owner: owner.clone(),
                key: key.into(),
                work,
                status: Status {
                    id,
                    state: State::Queued,
                    attempt: 0,
                },
                lease_until: None,
            });
            Ok((Receipt { id }, true))
        })
    }

    fn status(&self, owner: &Owner, id: JobId) -> Result<Status, JobError> {
        self.access(|data| Ok((record(data, owner, id)?.status, false)))
    }

    fn cancel(&self, owner: &Owner, id: JobId) -> Result<Status, JobError> {
        self.access(|data| {
            let record = record(data, owner, id)?;
            let changed = matches!(record.status.state, State::Queued | State::Running);
            if changed {
                record.status.state = State::Cancelled;
                record.lease_until = None;
            }
            Ok((record.status, changed))
        })
    }

    fn claim(&self, now: u64, lease: u64, retries: u32) -> Result<Option<Claim>, JobError> {
        let until = deadline(now, lease)?;
        let max_attempts = retries
            .checked_add(1)
            .ok_or(JobError::InvalidConfiguration)?;
        self.access(|data| {
            let mut changed = false;
            for record in &mut data.jobs {
                if record.lease_until.is_some_and(|until| until <= now) {
                    record.lease_until = None;
                    record.status.state = if record.status.attempt < max_attempts {
                        State::Queued
                    } else {
                        State::Failed
                    };
                    changed = true;
                }
            }
            let Some(record) = data
                .jobs
                .iter_mut()
                .find(|record| record.status.state == State::Queued)
            else {
                return Ok((None, changed));
            };
            record.status.attempt += 1;
            record.status.state = State::Running;
            record.lease_until = Some(until);
            Ok((
                Some(Claim {
                    id: record.status.id,
                    attempt: record.status.attempt,
                    work: record.work,
                }),
                true,
            ))
        })
    }

    fn renew(&self, claim: Claim, now: u64, lease: u64) -> Result<(), JobError> {
        let until = deadline(now, lease)?;
        self.access(|data| {
            current(data, claim, now)?.lease_until = Some(until);
            Ok(((), true))
        })
    }

    fn finish(&self, claim: Claim, now: u64, result: u64) -> Result<(), JobError> {
        self.access(|data| {
            let record = current(data, claim, now)?;
            record.status.state = State::Succeeded { result };
            record.lease_until = None;
            Ok(((), true))
        })
    }
}
