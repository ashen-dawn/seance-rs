use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use twilight_gateway::{Intents, Shard, ShardId};
use twilight_http::Client;
use twilight_model::gateway::{
    payload::outgoing::{update_presence::UpdatePresencePayload, UpdatePresence},
    OpCode,
};

use super::{MemberId, MessageEvent, Status, SystemEvent, UserId};

pub struct Gateway {
    member_id: MemberId,
    discord_token: String,
    reference_user_id: UserId,
    message_handler: Option<Arc<Mutex<Sender<MessageEvent>>>>,
    system_handler: Option<Arc<Mutex<Sender<SystemEvent>>>>,
    shard: Arc<Mutex<Shard>>,
}

impl Gateway {
    pub fn new(
        member_id: MemberId,
        config: &crate::config::Member,
        reference_user_id: UserId,
    ) -> Self {
        let intents = Intents::GUILD_MEMBERS
            | Intents::GUILD_PRESENCES
            | Intents::GUILD_MESSAGES
            | Intents::MESSAGE_CONTENT;

        Self {
            member_id,
            discord_token: config.discord_token.clone(),
            reference_user_id,
            message_handler: None,
            system_handler: None,
            shard: Arc::new(Mutex::new(Shard::new(
                ShardId::ONE,
                config.discord_token.clone(),
                intents,
            ))),
        }
    }

    pub fn set_message_handler(&mut self, handler: Sender<MessageEvent>) {
        self.message_handler = Some(Arc::new(Mutex::new(handler)));
    }

    pub fn set_system_handler(&mut self, handler: Sender<SystemEvent>) {
        self.system_handler = Some(Arc::new(Mutex::new(handler)));
    }

    pub async fn set_status(&self, status: Status) {
        let mut shard = self.shard.lock().await;

        shard
            .command(&UpdatePresence {
                d: UpdatePresencePayload {
                    activities: Vec::new(),
                    afk: false,
                    since: None,
                    status,
                },
                op: OpCode::PresenceUpdate,
            })
            .await
            .expect("Could not send command to gateway");
    }

    pub fn start_listening(&self) {
        let message_channel = self.message_handler.clone();
        let system_channel = self.system_handler.clone();
        let shard = self.shard.clone();
        let member_id = self.member_id.clone();
        let reference_user_id = self.reference_user_id.clone();
        let client = Client::new(self.discord_token.clone());

        tokio::spawn(async move {
            loop {
                let next_event = { shard.lock().await.next_event().await };

                match next_event {
                    Err(source) => {
                        if let Some(channel) = &system_channel {
                            let channel = channel.lock().await;

                            channel
                                .send(SystemEvent::GatewayError(member_id, source.to_string()))
                                .await;

                            if source.is_fatal() {
                                channel.send(SystemEvent::GatewayClosed(member_id)).await;
                                break;
                            }
                        }
                        todo!("Handle this")
                    }
                    Ok(event) => match event {
                        twilight_gateway::Event::Ready(_) => {
                            if let Some(channel) = &system_channel {
                                channel
                                    .lock()
                                    .await
                                    .send(SystemEvent::GatewayConnected(member_id))
                                    .await;
                            }
                        }

                        twilight_gateway::Event::MessageCreate(message_create) => {
                            let message = message_create.0;

                            if message.author.id != reference_user_id {
                                continue;
                            }

                            if let Some(channel) = &message_channel {
                                channel
                                    .lock()
                                    .await
                                    .send((message.timestamp, message))
                                    .await;
                            }
                        }

                        twilight_gateway::Event::MessageUpdate(message_update) => {
                            if message_update.author.is_none()
                                || message_update.author.as_ref().unwrap().id != reference_user_id
                            {
                                continue;
                            }

                            if message_update.edited_timestamp.is_none() {
                                println!("Message update but no edit timestamp");
                                continue;
                            }

                            if message_update.content.is_none() {
                                println!("Message update but no content");
                                continue;
                            }

                            let message = client
                                .message(message_update.channel_id, message_update.id)
                                .await
                                .expect("Could not load message")
                                .model()
                                .await
                                .expect("Could not deserialize message");

                            if let Some(channel) = &message_channel {
                                channel
                                    .lock()
                                    .await
                                    .send((message_update.edited_timestamp.unwrap(), message))
                                    .await;
                            }
                        }

                        _ => (),
                    },
                }
            }
        });
    }
}
