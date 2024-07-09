use tokio::sync::mpsc::Sender;
use twilight_model::{id::{Id, marker::{MessageMarker, UserMarker}}, user::User};
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
        println!("Starting client with token: {}", self.config.discord_token);

        let intents = Intents::GUILD_MEMBERS | Intents::GUILD_PRESENCES | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;

        let mut shard = Shard::new(ShardId::ONE, self.config.discord_token.clone(), intents);
        
        loop {
            match shard.next_event().await {
                Ok(event) => {
                    match event {
                        twilight_gateway::Event::Ready(client) => {
                            println!("Bot started for {}#{}", client.user.name, client.user.discriminator);
                        },

                        twilight_gateway::Event::MessageCreate(message) => {
                            if message.author.id != self.reference_user_id {
                                continue
                            }

                            if let Err(_) = channel.send(ClientEvent::Message {
                                event_time: message.timestamp,
                                message_id: message.id,
                                author: message.author.clone(),
                                content: message.content.clone()
                            }).await {
                                println!("Client listener error: System context has already closed");
                                return
                            }
                        },

                        twilight_gateway::Event::MessageUpdate(message) => {
                            if message.author.is_none() || message.author.as_ref().unwrap().id != self.reference_user_id {
                                continue
                            }

                            if message.edited_timestamp.is_none() {
                                println!("Message update but no edit timestamp");
                                continue;
                            }


                            if let Err(_) = channel.send(ClientEvent::Message {
                                event_time: message.edited_timestamp.unwrap(),
                                message_id: message.id,
                                author: message.author.unwrap(),
                                content: message.content.unwrap()
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
        message_id: Id<MessageMarker>,
        author: User,
        content: String,
    },
    Error(ReceiveMessageError)
}
