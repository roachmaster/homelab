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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_iterate_in_order_with_exact_sequences() {
        let mut ring = Ring::<u32, 4>::new();
        assert_eq!(ring.iter().collect::<Vec<_>>(), Vec::<u32>::new());
        assert_eq!((ring.latest(), ring.dropped()), (0, 0));
        for _ in 0..3 {
            ring.push(|sequence| sequence * 10);
        }
        assert_eq!(ring.iter().collect::<Vec<_>>(), vec![10, 20, 30]);
        assert_eq!((ring.latest(), ring.dropped()), (3, 0));
    }

    #[test]
    fn overflow_drops_the_oldest_and_counts_exactly() {
        let mut ring = Ring::<u32, 4>::new();
        for _ in 0..6 {
            ring.push(|sequence| sequence);
        }
        assert_eq!(ring.iter().collect::<Vec<_>>(), vec![3, 4, 5, 6]);
        assert_eq!((ring.latest(), ring.dropped()), (6, 2));
        ring.push(|sequence| sequence);
        assert_eq!(ring.iter().collect::<Vec<_>>(), vec![4, 5, 6, 7]);
        assert_eq!((ring.latest(), ring.dropped()), (7, 3));
    }

    #[test]
    fn identical_pushes_produce_identical_rings() {
        fn build() -> Vec<u32> {
            let mut ring = Ring::<u32, 3>::new();
            for _ in 0..5 {
                ring.push(|sequence| sequence * 2);
            }
            ring.iter().collect()
        }
        assert_eq!(build(), vec![6, 8, 10]);
        assert_eq!(build(), build());
    }
}
