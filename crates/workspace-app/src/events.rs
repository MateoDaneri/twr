//! Preserve channel backpressure while coalescing optional UI wake notifications.
use std::sync::mpsc;
enum Queue<T> {
    Unbounded(mpsc::Sender<T>),
    Bounded(mpsc::SyncSender<T>),
}
pub struct Sender<T> {
    queue: Queue<T>,
    wake: Option<async_channel::Sender<()>>,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let queue = match &self.queue {
            Queue::Unbounded(tx) => Queue::Unbounded(tx.clone()),
            Queue::Bounded(tx) => Queue::Bounded(tx.clone()),
        };
        Self {
            queue,
            wake: self.wake.clone(),
        }
    }
}
impl<T> Sender<T> {
    fn notify(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }
    pub fn send(&self, value: T) -> Result<(), mpsc::SendError<T>> {
        match &self.queue {
            Queue::Unbounded(tx) => tx.send(value),
            Queue::Bounded(tx) => tx.send(value),
        }?;
        self.notify();
        Ok(())
    }
    pub fn try_send(&self, value: T) -> Result<(), mpsc::TrySendError<T>> {
        match &self.queue {
            Queue::Unbounded(tx) => tx
                .send(value)
                .map_err(|e| mpsc::TrySendError::Disconnected(e.0)),
            Queue::Bounded(tx) => tx.try_send(value),
        }?;
        self.notify();
        Ok(())
    }
}
pub fn channel<T>(wake: Option<async_channel::Sender<()>>) -> (Sender<T>, mpsc::Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    (
        Sender {
            queue: Queue::Unbounded(tx),
            wake,
        },
        rx,
    )
}
pub fn sync_channel<T>(
    capacity: usize,
    wake: Option<async_channel::Sender<()>>,
) -> (Sender<T>, mpsc::Receiver<T>) {
    // Zero-capacity rendezvous requires a receiver running before notification.
    assert!(
        capacity > 0,
        "notifying channels require a positive capacity"
    );
    let (tx, rx) = mpsc::sync_channel(capacity);
    (
        Sender {
            queue: Queue::Bounded(tx),
            wake,
        },
        rx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wake_is_coalesced_after_delivery_without_losing_bounded_or_closed_channel_semantics() {
        let (wake, woken) = async_channel::bounded(1);
        let (tx, rx) = sync_channel(2, Some(wake));
        tx.try_send(1).unwrap();
        tx.send(2).unwrap();
        assert!(matches!(tx.try_send(3), Err(mpsc::TrySendError::Full(3))));
        assert_eq!(woken.try_recv(), Ok(()));
        assert!(woken.try_recv().is_err());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![1, 2]);
        drop(rx);
        assert!(tx.send(4).is_err());
        assert!(woken.try_recv().is_err());
        let (wake, woken) = async_channel::bounded(1);
        let (tx, rx) = channel(Some(wake));
        drop(woken);
        tx.clone().send("still delivered").unwrap();
        assert_eq!(rx.recv().unwrap(), "still delivered");
    }
}
