pub(super) struct Ring<T, const CAPACITY: usize> {
    records: [Option<T>; CAPACITY],
    first: usize,
    len: usize,
    latest: u32,
    dropped: u32,
}

impl<T: Copy, const CAPACITY: usize> Ring<T, CAPACITY> {
    pub(super) fn new() -> Self {
        Self {
            records: [None; CAPACITY],
            first: 0,
            len: 0,
            latest: 0,
            dropped: 0,
        }
    }

    pub(super) fn push(&mut self, record: impl FnOnce(u32) -> T) {
        let Some(sequence) = self.latest.checked_add(1) else {
            self.dropped = self.dropped.saturating_add(1);
            return;
        };
        self.records[(self.first + self.len) % CAPACITY] = Some(record(sequence));
        self.latest = sequence;
        if self.len == CAPACITY {
            self.first = (self.first + 1) % CAPACITY;
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.len += 1;
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = T> + '_ {
        (0..self.len).filter_map(|offset| self.records[(self.first + offset) % CAPACITY])
    }

    pub(super) fn latest(&self) -> u32 {
        self.latest
    }

    pub(super) fn dropped(&self) -> u32 {
        self.dropped
    }
}
