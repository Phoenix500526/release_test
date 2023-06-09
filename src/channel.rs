use crate::conflict_queue::ConflictQueue;
use crate::errors::ChannelError;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};

/// message id
pub type MSGID = usize;

#[allow(dead_code)]
/// Message Definition
#[derive(Debug)]
pub struct Message<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// a message has one or several keys
    keys: Vec<K>,
    /// message content
    content: T,
    /// underlying channel instance
    shared: Arc<Shared<K, T>>,
    /// message id
    id: MSGID,
}

impl<K, T> Message<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// Create a new message
    fn new(keys: Vec<K>, content: T, shared: Arc<Shared<K, T>>, id: MSGID) -> Self {
        Self {
            keys,
            content,
            shared,
            id,
        }
    }

    /// return a iterator over message's keys
    #[inline]
    pub fn keys(&self) -> std::slice::Iter<'_, K> {
        self.keys.iter()
    }

    /// return a content reference of the message
    // #[allow(dead_code)]
    #[inline]
    pub fn content(&self) -> &T {
        &self.content
    }

    /// return a id of the message
    #[inline]
    pub fn id(&self) -> MSGID {
        self.id
    }
}

impl<K, T> Drop for Message<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    #[inline]
    fn drop(&mut self) {
        match self.shared.queue.lock() {
            Ok(mut inner) => {
                let empty = inner.is_empty();
                let mut res = false;
                let id = self.id();

                for key in self.keys() {
                    res |= inner.deactivate(id, key);
                }

                if empty && res {
                    self.shared.available.notify_one();
                }
            }
            Err(e) => panic!("Poisoned Lock: {e:?}",),
        }
    }
}

/// Share a `VecDeque` between senders and a receiver
#[derive(Debug)]
struct Shared<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// channel queue
    queue: Mutex<ConflictQueue<K, T>>,
    /// channel conditional variable
    available: Condvar,
    /// bounded channel capacity
    capacity: Arc<usize>,
    /// sender counter
    senders: AtomicUsize,
    /// receiver drop flag
    receiver: AtomicBool,
}

impl<K, T> Shared<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// Create a new `Shared`
    fn new(cap: usize) -> Self {
        Self {
            queue: Mutex::new(ConflictQueue::with_capacity(cap)),
            available: Condvar::new(),
            capacity: Arc::new(cap),
            senders: AtomicUsize::new(1),
            receiver: AtomicBool::new(true),
        }
    }
}

/// Sender Definition
#[derive(Debug)]
pub struct Sender<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// channel inner data struct handler
    shared: Arc<Shared<K, T>>,
}

impl<K, T> Sender<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// Send a message to the channel
    ///
    /// # Errors
    /// If the channel is closed(the receiver is dropped), then calling this method will return a `ChannelError::NoReceiverLeft`
    /// If the Mutex of the channel is poisoned, then calling this method will return a `ChannelError::PoisonedLock`
    #[inline]
    pub fn send(&mut self, keys: Vec<K>, t: T) -> Result<(), ChannelError> {
        if self.is_closed() {
            return Err(ChannelError::NoReceiverLeft);
        }
        let mut inner = self.shared.queue.lock().map_err(|err| {
            let err: ChannelError = err.into();
            err
        })?;

        if inner.capacity() >= *self.shared.capacity {
            inner = self
                .shared
                .available
                .wait_while(inner, |pending| pending.capacity() >= *self.shared.capacity)
                .map_err(|err| {
                    let err: ChannelError = err.into();
                    err
                })?;
        }

        let was_empty = {
            let empty_before = inner.is_empty();
            let msg = Message::new(keys, t, Arc::clone(&self.shared), inner.generate_id());
            inner.add_msg(msg);
            empty_before && !inner.is_empty()
        };

        if was_empty {
            self.shared.available.notify_one();
        }

        Ok(())
    }

    /// get the self.shared.queue length
    ///
    /// # Panics
    /// Panics if the mutex lock is poisoned.
    #[must_use]
    #[inline]
    pub fn total_queued_items(&self) -> usize {
        match self.shared.queue.lock() {
            Ok(inner) => inner.capacity(),
            Err(p_err) => {
                let _data = p_err.get_ref();
                panic!("lock poisoned");
            }
        }
    }

    /// return true if the receiver is dropped
    #[must_use]
    #[inline]
    pub fn is_closed(&self) -> bool {
        !self.shared.receiver.load(Ordering::Acquire)
    }
}

impl<K, T> Clone for Sender<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    #[inline]
    fn clone(&self) -> Self {
        let _ = self.shared.senders.fetch_add(1, Ordering::AcqRel);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<K, T> Drop for Sender<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// Let's think about a conner case: what will happend when a receiver is already blocked on the channel while the last
    /// sender is dropped at the same time? This receiver is gone, we will never get a chance to wake it up.
    #[inline]
    fn drop(&mut self) {
        let old = self.shared.senders.fetch_sub(1, Ordering::AcqRel);
        if old <= 1 {
            self.shared.available.notify_all();
        }
    }
}

/// Receiver Definition
#[derive(Debug)]
pub struct Receiver<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// channel inner data struct handler
    shared: Arc<Shared<K, T>>,
}

impl<K, T> Receiver<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    /// Receive a message from the receiver
    ///
    /// # Errors
    /// If the last sender is dropped, then calling this function will return a `ChannelError::NoSenderLeft`
    #[inline]
    pub fn recv(&mut self) -> Result<Message<K, T>, ChannelError> {
        let mut inner = self.shared.queue.lock().map_err(|err| {
            let err: ChannelError = err.into();
            err
        })?;
        loop {
            match inner.pop_front() {
                Some(t) => {
                    if let Some(capacity_upperbound) = self.shared.capacity.checked_sub(1) {
                        if inner.capacity() == capacity_upperbound {
                            self.shared.available.notify_one();
                        }
                    }
                    return Ok(t);
                }
                None if self.total_senders() == 0 => return Err(ChannelError::NoSenderLeft),
                // When Condvar has been waken up, it will return MutexGuard. We can loop back to fetch a new message.
                None => {
                    inner = self.shared.available.wait(inner).map_err(|err| {
                        let err: ChannelError = err.into();
                        err
                    })?;
                }
            }
        }
    }

    /// return the amount of current alive senders.
    #[must_use]
    #[inline]
    pub fn total_senders(&self) -> usize {
        self.shared.senders.load(Ordering::Acquire)
    }
}

impl<K, T> Drop for Receiver<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    #[inline]
    fn drop(&mut self) {
        let _ = self.shared.receiver.fetch_and(false, Ordering::AcqRel);
    }
}

impl<K, T> Iterator for Receiver<K, T>
where
    K: Clone + Eq + Hash + Debug,
{
    type Item = Message<K, T>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.recv().ok()
    }
}

/// create a new bounded channel
///
/// # Panics
/// Panics if `cap` is less than or equal to zero
#[must_use]
#[inline]
pub fn bounded<K, T>(cap: usize) -> (Sender<K, T>, Receiver<K, T>)
where
    K: Clone + Eq + Hash + Debug,
{
    assert!(cap > 0);
    let shared = Shared::new(cap);
    let shared = Arc::new(shared);
    (
        Sender {
            shared: Arc::<Shared<K, T>>::clone(&shared),
        },
        Receiver { shared },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Debug;
    use std::thread;
    use std::time::Duration;

    fn error_handle<T, E>(res: Result<T, E>) -> T
    where
        E: Debug,
    {
        match res {
            Ok(t) => t,
            Err(e) => {
                unreachable!("{:?}", e)
            }
        }
    }

    #[test]
    fn channel_should_work() {
        let (mut s, mut r) = bounded(3);
        error_handle(s.send(vec![1, 2, 3], "hello world!".to_owned()));
        let msg = error_handle(r.recv());
        assert_eq!(msg.content().clone(), "hello world!".to_owned());
        assert_eq!(msg.keys().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn multiple_senders_and_one_receiver_should_work() {
        let (s, mut r) = bounded(3);
        let s1 = s.clone();
        let s2 = s.clone();
        for (idx, mut sender) in [s, s1, s2].into_iter().enumerate() {
            error_handle(
                thread::spawn(move || {
                    error_handle(sender.send(vec![idx], idx + 1));
                })
                .join(),
            );
        }
        let mut result = [r.recv(), r.recv(), r.recv()].map(|v| *error_handle(v).content());
        result.sort_unstable();
        assert_eq!(result, [1, 2, 3]);
    }

    #[test]
    fn receiver_should_be_blocked_when_the_channel_is_empty() {
        let (mut s, mut r) = bounded::<usize, usize>(10);
        let mut s1 = s.clone();
        let (mut s_latch, mut r_latch) = bounded::<usize, usize>(1);
        let _res = thread::spawn(move || {
            for idx in 0.. {
                match r.recv() {
                    Ok(msg) => assert_eq!(idx, *msg.content()),
                    Err(e) => match e {
                        ChannelError::NoSenderLeft => (),
                        ChannelError::NoReceiverLeft | ChannelError::PoisonedLock => {
                            unreachable!("Oops! Something went wrong")
                        }
                    },
                }
                if idx == 9 {
                    error_handle(s_latch.send(vec![0], 0));
                }
            }
            unreachable!();
        });

        let _res1 = thread::spawn(move || {
            for i in 0..10usize {
                error_handle(s.send(vec![i], i));
            }
        });

        let _res2 = error_handle(r_latch.recv());

        for i in 10..20usize {
            error_handle(s1.send(vec![i], i));
        }

        thread::sleep(Duration::from_millis(5));
        assert_eq!(s1.total_queued_items(), 0);
    }

    #[test]
    fn sender_should_be_blocked_when_the_channel_is_available() {
        let (s, mut r) = bounded(3);
        let mut s1 = s.clone();
        let cnt = Arc::new(AtomicUsize::new(0));
        let cnt_1 = Arc::clone(&cnt);

        let (mut s_latch, mut r_latch) = bounded(1);
        let _t = thread::spawn(move || {
            for i in 0..10 {
                error_handle(s1.send(vec![1, 2], i));
                let _ = cnt_1.fetch_add(1, Ordering::AcqRel);
                if i == 2 || i == 4 {
                    error_handle(s_latch.send(vec![i], 1));
                }
            }
            unreachable!();
        });

        let _res1 = error_handle(r_latch.recv());
        assert_eq!(3, cnt.load(Ordering::Acquire));
        for i in 0..2 {
            assert_eq!(i, error_handle(r.recv()).content().clone());
        }

        let _res2 = error_handle(r_latch.recv());

        assert_eq!(5, cnt.load(Ordering::Acquire));
        assert_eq!(2, error_handle(r.recv()).content().clone());

        thread::sleep(Duration::from_millis(1));
        assert_eq!(s.total_queued_items(), 3);
    }

    #[test]
    fn receive_error_when_the_last_sender_is_dropped() {
        let (s, mut r) = bounded(3);
        let s1 = s.clone();
        let senders = [s, s1];
        let total = senders.len();

        // drop the sender immediately after sending a message
        for mut sender in senders {
            error_handle(
                thread::spawn(move || {
                    error_handle(sender.send(vec![1], "hello".to_owned()));
                })
                .join(),
            );
        }

        // receiver will receive all the messages from the channel
        for _i in 0..total {
            let _res = error_handle(r.recv());
        }

        assert!(r.recv().is_err());
    }

    #[test]
    fn sender_get_error_when_the_receiver_is_dropped() {
        let (mut s1, mut s2) = {
            let (s, _) = bounded(3);
            let s1 = s.clone();
            (s, s1)
        };
        assert!(s1.is_closed());
        assert_eq!(s1.send(vec![1], 1), Err(ChannelError::NoReceiverLeft));
        assert!(s2.is_closed());
        assert_eq!(s2.send(vec![1], 1), Err(ChannelError::NoReceiverLeft));
    }

    #[test]
    fn a_blocked_receiver_should_be_waken_up_when_the_last_sender_dropped() {
        let (s, mut r) = bounded::<i32, i32>(1);
        // sender and reveiver is synchronized latch.
        let (mut sender, mut receiver) = bounded(1);

        let t1 = thread::spawn(move || {
            error_handle(sender.send(vec![1], 1));
            match r.recv() {
                Ok(_) => unreachable!(),
                Err(e) => assert_eq!(e, ChannelError::NoSenderLeft),
            }
        });

        let _t = thread::spawn(move || {
            let _res = error_handle(receiver.recv()); // make sure that r.recv() will execute before drop(s)
            drop(s);
        });
        error_handle(t1.join()); // otherwise, here will be blocked and never finish
    }

    #[test]
    fn receiver_will_get_correct_sequence_when_message_conflict() {
        let (mut s, mut r) = bounded(6);
        error_handle(s.send(vec![1, 2, 3], "Message A".to_owned()));
        error_handle(s.send(vec![5, 6], "Message B".to_owned()));
        error_handle(s.send(vec![2, 3, 5], "Message C".to_owned()));
        error_handle(s.send(vec![2], "Message D".to_owned()));
        error_handle(s.send(vec![5], "Message E".to_owned()));
        error_handle(s.send(vec![1], "Message F".to_owned()));

        let msg_1 = error_handle(r.recv());
        let msg_2 = error_handle(r.recv());
        assert_eq!(msg_1.content().clone(), "Message A".to_owned());
        assert_eq!(msg_1.keys().copied().collect::<Vec<_>>(), vec![1, 2, 3]);

        assert_eq!(msg_2.content().clone(), "Message B".to_owned());
        assert_eq!(msg_2.keys().copied().collect::<Vec<_>>(), vec![5, 6]);

        error_handle(s.send(vec![7], "Message G".to_owned()));
        drop(msg_1);
        error_handle(s.send(vec![8], "Message H".to_owned()));

        let msg_3 = error_handle(r.recv());
        assert_eq!(msg_3.content().clone(), "Message G".to_owned());
        assert_eq!(msg_3.keys().copied().collect::<Vec<_>>(), vec![7]);

        let msg_4 = error_handle(r.recv());
        assert_eq!(msg_4.content().clone(), "Message F".to_owned());
        assert_eq!(msg_4.keys().copied().collect::<Vec<_>>(), vec![1]);

        let msg_5 = error_handle(r.recv());
        assert_eq!(msg_5.content().clone(), "Message H".to_owned());
        assert_eq!(msg_5.keys().copied().collect::<Vec<_>>(), vec![8]);

        drop(msg_2);
        let msg_6 = error_handle(r.recv());
        assert_eq!(msg_6.content().clone(), "Message C".to_owned());
        assert_eq!(msg_6.keys().copied().collect::<Vec<_>>(), vec![2, 3, 5]);

        drop(msg_6);
        let msg_7 = error_handle(r.recv());
        assert_eq!(msg_7.content().clone(), "Message D".to_owned());
        assert_eq!(msg_7.keys().copied().collect::<Vec<_>>(), vec![2]);

        let msg_8 = error_handle(r.recv());
        assert_eq!(msg_8.content().clone(), "Message E".to_owned());
        assert_eq!(msg_8.keys().copied().collect::<Vec<_>>(), vec![5]);
    }
}
