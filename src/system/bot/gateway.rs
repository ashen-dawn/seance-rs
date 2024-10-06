use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::Mutex;
use twilight_model::gateway::OpCode;
use twilight_model::gateway::payload::outgoing::{update_presence::UpdatePresencePayload, UpdatePresence};
use twilight_gateway::{
    Intents, Shard, ShardId, 
};

use super::{Message, Status, SystemEvent, BotConfig};

pub struct Gateway {
    shard: Arc<Mutex<Shard>>,
    bot_conf: Arc<RwLock<BotConfig>>,
}

impl Gateway {
    pub fn new(discord_token: &String, bot_conf: &Arc<RwLock<BotConfig>>) -> Self {
        let intents = Intents::GUILD_MEMBERS
            | Intents::GUILD_PRESENCES
            | Intents::GUILD_MESSAGES
            | Intents::MESSAGE_CONTENT;

        Self {
            shard: Arc::new(Mutex::new(Shard::new(
                ShardId::ONE,
                discord_token.clone(),
                intents,
            ))),
            bot_conf: bot_conf.clone(),
        }
    }

    pub async fn set_status(&self, status: Status) {
        {
            let last_status = { (*self.bot_conf.read().await).last_status };

            if status == last_status {
                return
            }
        }


        {
            let mut shard = self.shard.lock().await;

            shard.command(&UpdatePresence {
                d: UpdatePresencePayload {
                    activities: Vec::new(),
                    afk: false,
                    since: None,
                    status,
                },
                op: OpCode::PresenceUpdate,
            }).await.expect("Could not send command to gateway");
        }

        self.bot_conf.write().await.last_status = status;
    }

    pub fn start_listening(&self) {
        let bot_conf = self.bot_conf.clone();
        let shard = self.shard.clone();
        tokio::spawn(async move {
            loop {
                let bot_conf = { (*bot_conf.read().await).clone() };
                let next_event = { shard.lock().await.next_event().await };
                let system_channel = bot_conf.system_handler.as_ref().expect("No system channel");
                let message_channel = bot_conf.message_handler.as_ref().expect("No message channel");

                match next_event {
                    Err(source) => {
                        system_channel
                            .send(SystemEvent::GatewayError(bot_conf.member_id, source.to_string()))
                            .await;

                        if source.is_fatal() {
                            system_channel.send(SystemEvent::GatewayClosed(bot_conf.member_id)).await;
                            return;
                        }
                    }
                    Ok(event) => match event {
                        twilight_gateway::Event::Ready(_) => {
                            system_channel
                                .send(SystemEvent::GatewayConnected(bot_conf.member_id))
                                .await;
                        }

                        twilight_gateway::Event::MessageCreate(message_create) => {
                            let message = message_create.0;

                            if message.author.id != bot_conf.reference_user_id {
                                continue;
                            }

                            message_channel
                                .send((message.timestamp, Message::Complete(message, bot_conf.member_id)))
                                .await;
                        }

                        twilight_gateway::Event::MessageUpdate(message_update) => {
                            if message_update.author.is_none()
                                || message_update.author.as_ref().unwrap().id != bot_conf.reference_user_id
                            {
                                continue;
                            }

                            if message_update.edited_timestamp.is_none() || message_update.content.is_none() {
                                continue;
                            }

                            message_channel
                                .send((message_update.edited_timestamp.unwrap(), Message::Partial(*message_update, bot_conf.member_id)))
                                .await;
                        }

                        _ => (),
                    },
                };
            }
        });
    }
}
