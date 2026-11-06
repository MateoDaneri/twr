//! Bounded ownership of live terminal clients whose workspaces were closed.
//! Parking never drops a client to make room: callers retain it on failure.
use std::collections::HashMap;

pub const MAX_PARKED_TERMINALS: usize = 16;

pub struct Parked<T> {
    entries: HashMap<usize, T>,
    limit: usize,
}

impl<T> Default for Parked<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            limit: MAX_PARKED_TERMINALS,
        }
    }
}

impl<T> Parked<T> {
    pub fn iter(&self) -> impl Iterator<Item = (&usize, &T)> {
        self.entries.iter()
    }
    pub fn can_park(&self, project: usize) -> bool {
        !self.entries.contains_key(&project) && self.entries.len() < self.limit
    }
    pub fn park(&mut self, project: usize, client: T) -> Result<(), T> {
        if !self.can_park(project) {
            return Err(client);
        }
        self.entries.insert(project, client);
        Ok(())
    }
    pub fn take(&mut self, project: usize) -> Option<T> {
        self.entries.remove(&project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Client(Arc<AtomicUsize>);
    impl Drop for Client {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn bounded_pool_preserves_clients_and_returns_rejected_owner() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut parked = Parked {
            entries: HashMap::new(),
            limit: 1,
        };
        assert!(parked.park(7, Client(drops.clone())).is_ok());
        let rejected = parked.park(8, Client(drops.clone())).err().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let restored = parked.take(7).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(parked.park(8, rejected).is_ok());
        drop(restored);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(parked);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn duplicate_project_never_replaces_the_existing_client() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut parked = Parked {
            entries: HashMap::new(),
            limit: 2,
        };
        assert!(parked.park(7, Client(drops.clone())).is_ok());
        let rejected = parked.park(7, Client(drops.clone())).err().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(parked.take(7).is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(rejected);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}
