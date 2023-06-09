use crate::channel::{Message, MSGID};
use hashbrown::{HashMap, HashSet};
use std::collections::VecDeque;
use std::fmt::Debug;
use std::hash::Hash;

/// conflict queue internal struct
#[derive(Debug)]
pub struct ConflictQueue<K, T>
where
    K: Clone + Eq + Debug + Hash,
{
    /// active messages index
    active: HashMap<K, HashSet<MSGID>>,
    /// `conflict_buffer` is used to store all those conflicted messages
    conflict_buffer: HashMap<MSGID, (usize, Message<K, T>)>,
    /// `fifo_queue` is used to store those unconflicted messages
    fifo_queue: VecDeque<Message<K, T>>,
    /// inner message id
    id: MSGID,
}

impl<K, T> ConflictQueue<K, T>
where
    K: Clone + Eq + Debug + Hash,
{
    /// Constructs a new, empty ConflictQueue<K, T> with at least the specified capacity.
    #[inline]
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            active: HashMap::new(),
            conflict_buffer: HashMap::with_capacity(cap),
            fifo_queue: VecDeque::with_capacity(cap),
            id: 0,
        }
    }

    /// generate a unique message id
    #[inline]
    pub fn generate_id(&mut self) -> MSGID {
        let old_id = self.id;
        if let Some(new_id) = self.id.checked_add(1) {
            self.id = new_id;
        } else {
            self.id = 0;
        }
        old_id
    }

    /// return the `fifo_queue` is empty or not
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fifo_queue.is_empty()
    }

    /// return the capacity of the conflict queue
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        let buffer_len = self.conflict_buffer.len();
        let fifo_len = self.fifo_queue.len();
        if let Some(capacity) = buffer_len.checked_add(fifo_len) {
            capacity
        } else {
            0
        }
    }

    /// return the length of the conflict queue
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.fifo_queue.len()
    }

    /// pop front message from the `fifo_queue`
    #[inline]
    pub fn pop_front(&mut self) -> Option<Message<K, T>> {
        self.fifo_queue.pop_front()
    }

    /// push a message to the tail of `fifo_queue`
    #[inline]
    pub fn push_back(&mut self, msg: Message<K, T>) {
        self.fifo_queue.push_back(msg);
    }

    /// add a message into the conflict queue
    #[inline]
    pub fn add_msg(&mut self, msg: Message<K, T>) {
        let id = msg.id();
        let mut indegree: usize = 0;
        for key in msg.keys() {
            let entry = self.active.entry(key.clone()).or_default();
            if let Some(new_indegree) = indegree.checked_add(entry.len()) {
                indegree = new_indegree;
            }
            let _ = entry.insert(id);
        }

        if indegree == 0 {
            self.fifo_queue.push_back(msg);
        } else {
            let _res = self.conflict_buffer.insert(id, (indegree, msg));
        }
    }

    /// deactivate a message
    /// return true if any conflict has been solved
    #[inline]
    pub fn deactivate(&mut self, msg_id: MSGID, key: &K) -> bool {
        let mut res = false;
        if let Some(activate) = self.active.get_mut(key) {
            let _ = activate.remove(&msg_id);
            for id in activate.iter() {
                if let Some((mut indegree, msg)) = self.conflict_buffer.remove(id) {
                    if indegree == 1 {
                        self.fifo_queue.push_back(msg);
                        res = true;
                    } else {
                        if let Some(new_indegree) = indegree.checked_sub(1) {
                            indegree = new_indegree;
                        }
                        let _res = self.conflict_buffer.insert(*id, (indegree, msg));
                    }
                }
            }
        }
        res
    }
}
