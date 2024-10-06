use lru::LruCache;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::num::NonZeroUsize;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use twilight_model::channel::Message as TwiMessage;

use super::{MemberId, Message as GatewayMessage, MessageEvent, MessageId, SystemEvent};

pub struct AggregatorState {
    rx: Receiver<MessageEvent>,
    tx: Sender<MessageEvent>,
    message_cache: lru::LruCache<MessageId, (TwiMessage, MemberId)>,
    system_emitter: Option<Sender<SystemEvent>>,
}

pub struct MessageAggregator {
    state: Arc<RwLock<AggregatorState>>,
}

impl MessageAggregator {
    pub fn new(system_size: usize) -> Self {
        let (tx, rx) = channel::<MessageEvent>(system_size * 2);

        Self {
            state: Arc::new(RwLock::new( AggregatorState {
                tx,
                rx,
                message_cache: LruCache::new(NonZeroUsize::new(system_size * 2).unwrap()),
                system_emitter: None,

            }))
        }
    }

    pub async fn get_sender(&self) -> Sender<MessageEvent> {
        self.state.read().await.tx.clone()
    }

    pub async fn set_system_handler(&mut self, emitter: Sender<SystemEvent>) -> () {
        self.state.write().await.system_emitter = Some(emitter);
    }

    // We probably don't actully need this since we've got a separate sent-cache by channel
    // pub async fn lookup_message(&self, message_id: MessageId) -> Option<TwiMessage> {
    //     self.state.write().await.message_cache.get(&message_id).map(|m| m.clone())
    // }

    pub fn start(&self) -> () {
        let state = self.state.clone();

        tokio::spawn(async move {
            loop {
                let system_emitter = { state.read().await.system_emitter.clone().expect("No system emitter") };
                let self_emitter = { state.read().await.tx.clone() };
                let next_event = { state.write().await.rx.recv().await };


                match next_event {
                    None => (),
                    Some((timestamp, message)) => {
                        match message {
                            GatewayMessage::Partial(current_partial, member_id) => {
                                let cache_content = { state.write().await.message_cache.get(&current_partial.id).map(|m| m.clone()) };
                                match cache_content {
                                    Some((original_message, member_id)) => {

                                        let mut updated_message = original_message.clone();
                                        if let Some(edited_time) = current_partial.edited_timestamp {
                                            updated_message.edited_timestamp = Some(edited_time);
                                        }

                                        if let Some(content) = &current_partial.content {
                                            updated_message.content = content.clone()
                                        }

                                        self_emitter.send((timestamp, GatewayMessage::Complete(updated_message, member_id))).await;
                                    },
                                    None => {
                                        system_emitter.send(
                                            SystemEvent::RefetchMessage(member_id, current_partial.id, current_partial.channel_id)
                                        ).await;
                                    },
                                };
                            },
                            GatewayMessage::Complete(message, member_id) => {
                                let previous_message = { state.write().await.message_cache.get(&message.id).map(|m| m.clone()) };

                                if let Some((previous_message, _last_seen_by)) = previous_message {
                                    let previous_timestamp = previous_message.edited_timestamp.unwrap_or(previous_message.timestamp);
                                    let current_timestamp = message.edited_timestamp.unwrap_or(message.timestamp);

                                    // Should we skip sending
                                    if previous_timestamp.as_micros() >= current_timestamp.as_micros() {
                                        continue
                                    }
                                    
                                    // If not, fall through to update stored message
                                }

                                { state.write().await.message_cache.put(message.id, (message.clone(), member_id)); };

                                system_emitter
                                    .send(SystemEvent::NewMessage(timestamp, message, member_id))
                                    .await;
                            },
                        };
                    }
                }
            }
        });
    }
}
