use lru::LruCache;
use std::num::NonZeroUsize;
use tokio::sync::mpsc::{channel, Receiver, Sender};

use super::{MessageEvent, MessageId, SystemEvent};

pub struct MessageAggregator {
    rx: Receiver<MessageEvent>,
    tx: Sender<MessageEvent>,
    message_cache: lru::LruCache<MessageId, MessageEvent>,
    emitter: Option<Sender<SystemEvent>>,
}

impl MessageAggregator {
    pub fn new() -> Self {
        let (tx, rx) = channel::<MessageEvent>(100);

        Self {
            tx,
            rx,
            message_cache: LruCache::new(NonZeroUsize::new(100).unwrap()),
            emitter: None,
        }
    }

    pub fn get_sender(&self) -> Sender<MessageEvent> {
        self.tx.clone()
    }

    pub fn set_handler(&mut self, emitter: Sender<SystemEvent>) -> () {
        self.emitter = Some(emitter);
    }

    pub fn start(mut self) -> () {
        tokio::spawn(async move {
            loop {
                match self.rx.recv().await {
                    None => return,
                    Some((timestamp, message)) => {
                        let last_seen_timestamp = self.message_cache.get(&message.id);
                        let current_timestamp = timestamp;

                        if last_seen_timestamp.is_none()
                            || last_seen_timestamp.unwrap().0.as_micros()
                                < current_timestamp.as_micros()
                        {
                            self.message_cache
                                .put(message.id, (timestamp, message.clone()));

                            if let Some(emitter) = &self.emitter {
                                emitter
                                    .send(SystemEvent::NewMessage((timestamp, message)))
                                    .await;
                            }
                        };
                    }
                }
            }
        });
    }
}
