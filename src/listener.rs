use tokio::sync::mpsc::Sender;
use twilight_http::Client;
use twilight_model::{channel::Message, id::{Id, marker::{ChannelMarker, MessageMarker, UserMarker}}, user::User};
use twilight_gateway::{error::ReceiveMessageError, Intents, Shard, ShardId};
use twilight_model::util::Timestamp;

pub struct Listener {
    pub config: crate::config::Member,
    pub reference_user_id: Id<UserMarker>,
}

impl Listener {
    pub fn new(config: crate::config::Member, reference_user_id: Id<UserMarker>) -> Self {
        Listener {
            config,
            reference_user_id,
        }
    }

    pub async fn start_listening(&mut self, channel : Sender<ClientEvent>) {
        let intents = Intents::GUILD_MEMBERS | Intents::GUILD_PRESENCES | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;

        let mut shard = Shard::new(ShardId::ONE, self.config.discord_token.clone(), intents);
        let mut client = Client::new(self.config.discord_token.clone());
        
        loop {
            match shard.next_event().await {
                Ok(event) => {
                    match event {
                        twilight_gateway::Event::Ready(client) => {
                            println!("Bot started for {}#{}", client.user.name, client.user.discriminator);
                        },

                        twilight_gateway::Event::MessageCreate(message_create) => {
                            let message = message_create.0;

                            if message.author.id != self.reference_user_id {
                                continue
                            }

                            if let Err(_) = channel.send(ClientEvent::Message {
                                event_time: message.timestamp,
                                message
                            }).await {
                                println!("Client listener error: System context has already closed");
                                return
                            }
                        },

                        twilight_gateway::Event::MessageUpdate(message_update) => {
                            if message_update.author.is_none() || message_update.author.as_ref().unwrap().id != self.reference_user_id {
                                continue
                            }

                            if message_update.edited_timestamp.is_none() {
                                println!("Message update but no edit timestamp");
                                continue;
                            }

                            if message_update.content.is_none() {
                                println!("Message update but no content");
                                continue;
                            }

                            let message = client.message(message_update.channel_id, message_update.id)
                                .await.expect("Could not load message")
                                .model().await.expect("Could not deserialize message");

                            if let Err(_) = channel.send(ClientEvent::Message {
                                event_time: message_update.edited_timestamp.unwrap(),
                                message,
                            }).await {
                                println!("Client listener error: System context has already closed");
                                return
                            }
                        },

                        _ => (),
                    }
                },
                Err(source) => {
                    if !source.is_fatal() {
                        println!("Warn: Client encountered an error: {}", source.to_string());
                        continue;
                    }

                    if let Err(_) = channel.send(ClientEvent::Error(source)).await {
                        println!("Client listener error: System context has already closed");
                        break;
                    }
                    break;
                }
            };
        }
    }
}

pub enum ClientEvent {
    Message {
        event_time: Timestamp,
        message: Message,
    },
    Error(ReceiveMessageError)
}
