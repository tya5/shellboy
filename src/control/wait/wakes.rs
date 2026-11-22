//! Owner-local, coalescing invalidations. The registry never stores predicate values.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Key {
    Reply(String),
    Turn(String),
    Run(String),
    ThreadSettled(String),
    ThreadUnavailable(String),
    ShellTerminal(String),
    ShellOutput(String),
    DisplayOutput,
    CallerInput(String),
    Foreground(String),
    Changed(Option<String>),
    Live(Option<String>),
    Reasoning(Option<String>),
    AttemptEnd(Option<String>),
    Config,
}

impl Key {
    pub(crate) fn recovery(thread: String, run: String, turn: String) -> [Self; 3] {
        [
            Self::ThreadSettled(thread),
            Self::Run(run),
            Self::Turn(turn),
        ]
    }
    pub(crate) fn shell_terminal(execution: String) -> [Self; 2] {
        [
            Self::ShellTerminal(execution.clone()),
            Self::ShellOutput(execution),
        ]
    }
}

#[derive(Default)]
struct Index {
    next: u64,
    by_key: HashMap<Key, HashMap<u64, Arc<Notify>>>,
    cursors: HashMap<u64, Cursor>,
}
struct Cursor {
    after: u64,
    text: bool,
    notify: Arc<Notify>,
}

#[derive(Clone, Default)]
pub(crate) struct Wakes(Arc<Mutex<Index>>);

pub(crate) struct Subscription {
    wakes: Wakes,
    id: u64,
    keys: Mutex<HashSet<Key>>,
    pub(crate) notify: Arc<Notify>,
}

impl Wakes {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        let index = self.0.lock().unwrap();
        index.by_key.is_empty() && index.cursors.is_empty()
    }

    pub(in crate::control) fn subscribe(
        &self,
        keys: impl IntoIterator<Item = Key>,
    ) -> Subscription {
        let id = {
            let mut index = self.0.lock().unwrap();
            index.next = index
                .next
                .checked_add(1)
                .expect("wait subscription ID exhausted");
            index.next
        };
        let subscription = Subscription {
            wakes: self.clone(),
            id,
            keys: Mutex::default(),
            notify: Arc::default(),
        };
        subscription.add(keys);
        subscription
    }

    pub(crate) fn publish(&self, keys: impl IntoIterator<Item = Key>) {
        self.publish_clock(keys, 0, 0);
    }

    pub(crate) fn publish_clock(
        &self,
        keys: impl IntoIterator<Item = Key>,
        floor: u64,
        end_floor: u64,
    ) {
        let listeners = {
            let index = self.0.lock().unwrap();
            let mut listeners = HashMap::new();
            for key in keys {
                if let Some(bucket) = index.by_key.get(&key) {
                    listeners.extend(bucket.iter().map(|(&id, notify)| (id, notify.clone())));
                }
            }
            if floor != 0 || end_floor != 0 {
                listeners.extend(
                    index
                        .cursors
                        .iter()
                        .filter(|(_, cursor)| {
                            cursor.after < floor || cursor.text && cursor.after < end_floor
                        })
                        .map(|(&id, cursor)| (id, cursor.notify.clone())),
                );
            }
            listeners
        };
        for notify in listeners.into_values() {
            notify.notify_one();
        }
    }
}

impl Subscription {
    pub(super) fn add(&self, keys: impl IntoIterator<Item = Key>) {
        let mut own = self.keys.lock().unwrap();
        let mut index = self.wakes.0.lock().unwrap();
        for key in keys {
            if own.insert(key.clone()) {
                index
                    .by_key
                    .entry(key)
                    .or_default()
                    .insert(self.id, self.notify.clone());
            }
        }
    }

    pub(in crate::control) fn cursor(&self, after: u64, text: bool) {
        self.wakes.0.lock().unwrap().cursors.insert(
            self.id,
            Cursor {
                after,
                text,
                notify: self.notify.clone(),
            },
        );
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut index = self.wakes.0.lock().unwrap();
        index.cursors.remove(&self.id);
        for key in self.keys.get_mut().unwrap().drain() {
            if let Some(bucket) = index.by_key.get_mut(&key) {
                bucket.remove(&self.id);
                if bucket.is_empty() {
                    index.by_key.remove(&key);
                }
            }
        }
    }
}
