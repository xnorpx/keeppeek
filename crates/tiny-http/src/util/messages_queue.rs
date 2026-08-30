use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

enum Control<T> {
    Elem(T),
    Unblock,
}

pub struct MessagesQueue<T>
where
    T: Send,
{
    queue: Mutex<VecDeque<Control<T>>>,
    condvar: Condvar,
    not_full: Condvar,
    capacity: usize,
}

impl<T> MessagesQueue<T>
where
    T: Send,
{
    pub fn with_capacity(capacity: usize) -> Arc<Self> {
        let capacity = capacity.max(1);
        Arc::new(Self {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            condvar: Condvar::new(),
            not_full: Condvar::new(),
            capacity,
        })
    }

    /// Pushes an element to the queue.
    pub fn push(&self, value: T) {
        let mut queue = self.queue.lock().unwrap();
        while queue.len() >= self.capacity {
            queue = self.not_full.wait(queue).unwrap();
        }
        queue.push_back(Control::Elem(value));
        self.condvar.notify_one();
    }

    /// Unblock one thread stuck in pop loop.
    pub fn unblock(&self) {
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(Control::Unblock);
        self.condvar.notify_one();
    }

    /// Pops an element. Blocks until one is available.
    /// Returns None in case unblock() was issued.
    pub fn pop(&self) -> Option<T> {
        let mut queue = self.queue.lock().unwrap();

        loop {
            match queue.pop_front() {
                Some(Control::Elem(value)) => {
                    self.not_full.notify_one();
                    return Some(value);
                }
                Some(Control::Unblock) => {
                    self.not_full.notify_one();
                    return None;
                }
                None => (),
            }

            queue = self.condvar.wait(queue).unwrap();
        }
    }

    /// Tries to pop an element without blocking.
    pub fn try_pop(&self) -> Option<T> {
        let mut queue = self.queue.lock().unwrap();
        match queue.pop_front() {
            Some(Control::Elem(value)) => {
                self.not_full.notify_one();
                Some(value)
            }
            Some(Control::Unblock) => {
                self.not_full.notify_one();
                None
            }
            None => None,
        }
    }

    /// Tries to pop an element without blocking
    /// more than the specified timeout duration
    /// or unblock() was issued
    pub fn pop_timeout(&self, timeout: Duration) -> Option<T> {
        let mut queue = self.queue.lock().unwrap();
        let mut duration = timeout;
        loop {
            match queue.pop_front() {
                Some(Control::Elem(value)) => {
                    self.not_full.notify_one();
                    return Some(value);
                }
                Some(Control::Unblock) => {
                    self.not_full.notify_one();
                    return None;
                }
                None => (),
            }
            let now = Instant::now();
            let (_queue, result) = self.condvar.wait_timeout(queue, duration).unwrap();
            queue = _queue;
            let sleep_time = now.elapsed();
            duration = if duration > sleep_time {
                duration - sleep_time
            } else {
                Duration::from_millis(0)
            };
            if result.timed_out()
                || (duration.as_secs() == 0 && duration.subsec_nanos() < 1_000_000)
            {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    #[test]
    fn configured_capacity_blocks_producers_until_an_item_is_removed() {
        let queue = MessagesQueue::with_capacity(1);
        queue.push(1);
        let producer_queue = queue.clone();
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let producer = thread::spawn(move || {
            producer_queue.push(2);
            finished_tx.send(()).unwrap();
        });

        assert!(finished_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(queue.pop(), Some(1));
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(queue.pop(), Some(2));
        producer.join().unwrap();
    }
}
