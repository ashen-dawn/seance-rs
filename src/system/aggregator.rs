use lru::LruCache;
use std::num::NonZeroUsize;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use twilight_model::channel::Message as TwiMessage;

use super::{Message as GatewayMessage, MessageEvent, MessageId, SystemEvent};

pub struct MessageAggregator {
    rx: Receiver<MessageEvent>,
    tx: Sender<MessageEvent>,
    message_cache: lru::LruCache<MessageId, TwiMessage>,
    system_emitter: Option<Sender<SystemEvent>>,
}

impl MessageAggregator {
    pub fn new() -> Self {
        let (tx, rx) = channel::<MessageEvent>(100);

        Self {
            tx,
            rx,
            message_cache: LruCache::new(NonZeroUsize::new(100).unwrap()),
            system_emitter: None,
        }
    }

    pub fn get_sender(&self) -> Sender<MessageEvent> {
        self.tx.clone()
    }

    pub fn set_system_handler(&mut self, emitter: Sender<SystemEvent>) -> () {
        self.system_emitter = Some(emitter);
    }

    pub fn start(mut self) -> () {
        tokio::spawn(async move {
            loop {
                match self.rx.recv().await {
                    None => (),
                    Some((timestamp, message)) => {
                        let system_emitter = &self.system_emitter.clone().expect("No system emitter");
                        match message {
                            GatewayMessage::Partial(current_partial, member_id) => {
                                match self.message_cache.get(&current_partial.id) {
                                    Some(original_message) => {

                                        let mut updated_message = original_message.clone();
                                        if let Some(edited_time) = current_partial.edited_timestamp {
                                            updated_message.edited_timestamp = Some(edited_time);
                                        }

                                        if let Some(content) = current_partial.content {
                                            updated_message.content = content
                                        }

                                        self.tx.send((timestamp, GatewayMessage::Complete(updated_message))).await;
                                    },
                                    None => {
                                        system_emitter.send(
                                            SystemEvent::RefetchMessage(member_id, current_partial.id, current_partial.channel_id)
                                        ).await;
                                    },
                                };
                            },
                            GatewayMessage::Complete(message) => {
                                let previous_message = self.message_cache.get(&message.id);

                                if let Some(previous_message) = previous_message.cloned() {
                                    let previous_timestamp = previous_message.edited_timestamp.unwrap_or(previous_message.timestamp);
                                    let current_timestamp = message.edited_timestamp.unwrap_or(message.timestamp);

                                    // Should we skip sending
                                    if previous_timestamp.as_micros() >= current_timestamp.as_micros() {
                                        continue
                                    }
                                    
                                    // If not, fall through to update stored message
                                }

                                self.message_cache.put(message.id, message.clone());

                                self.system_emitter.as_ref().expect("Aggregator has no system emitter")
                                    .send(SystemEvent::NewMessage(timestamp, message))
                                    .await;
                            },
                        };
                    }
                }
            }
        });
    }
}
