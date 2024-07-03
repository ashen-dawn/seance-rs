use std::{num::NonZeroUsize, str::FromStr};

use std::sync::Arc;
use tokio::task::JoinSet;
use tokio::sync::Mutex;
use twilight_http::Client;
use twilight_model::id::{Id, marker::{MessageMarker, UserMarker}};
use twilight_gateway::{Intents, Shard, ShardId};

#[derive(Clone)]
pub struct System {
    pub config: crate::config::System,
    pub message_dedup_cache: Arc<Mutex<lru::LruCache<Id<MessageMarker>, ()>>>,
}

impl System {
    pub fn new(system_config: crate::config::System) -> Self {
        System {
            config: system_config,
            message_dedup_cache: Arc::new(Mutex::new(lru::LruCache::new(NonZeroUsize::new(100).unwrap())))
        }
    }

    pub async fn start_clients(&mut self) {
        println!("Starting clients for system");

        let reference_user_id : Id<UserMarker> = Id::from_str(self.config.reference_user_id.as_str())
            .expect(format!("Invalid user ID: {}", self.config.reference_user_id).as_str());

        let mut set = JoinSet::new();

        for member in self.config.members.iter() {
            let token = member.discord_token.clone();
            let self_clone = self.clone();
            set.spawn(async move {
                println!("Starting client with token: {}", token);

                let intents = Intents::GUILD_MEMBERS | Intents::GUILD_PRESENCES | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;

                let mut shard = Shard::new(ShardId::ONE, token.clone(), intents);
                let mut client = Client::new(token.clone());

                loop {
                    let event = match shard.next_event().await {
                        Ok(event) => event,
                        Err(source) => {
                            println!("error receiving event");

                            if source.is_fatal() {
                                break;
                            }

                            continue;
                        }
                    };

                    match event {
                        twilight_gateway::Event::Ready(client) => {
                            println!("Bot started for {}#{}", client.user.name, client.user.discriminator);
                        },

                        twilight_gateway::Event::MessageCreate(message) => {
                            if message.author.id != reference_user_id {
                                continue
                            }

                            if self_clone.is_new_message(message.id).await {
                                self_clone.handle_message_create(message, &mut client).await;
                            }
                        },

                        twilight_gateway::Event::MessageUpdate(message) => {
                            if message.author.is_none() || message.author.as_ref().unwrap().id != reference_user_id {
                                continue
                            }

                            if self_clone.is_new_message(message.id).await {
                                self_clone.handle_message_update(message, &mut client).await;
                            }
                        },

                        _ => (),
                    }
                }
            });
        }

        while let Some(join_result) = set.join_next().await {
            if let Err(join_error) = join_result {
                println!("Task encountered error: {}", join_error);
            } else {
                println!("Task joined cleanly");
            }
        }
    }

    async fn is_new_message(&self, message_id: Id<MessageMarker>) -> bool {
        let mut message_cache = self.message_dedup_cache.lock().await;
        if let None = message_cache.get(&message_id) {
            message_cache.put(message_id, ());
            true
        } else {
            false
        }
    }

    async fn handle_message_create(&self, message: Box<twilight_model::gateway::payload::incoming::MessageCreate>, client: &mut Client) {
        println!("Message created: {}", message.content);

        if let Err(err) = client.create_message(message.channel_id)
            .reply(message.id)
            .content(&format!("Recognized message from authorized user {}", message.author.name)).expect("Error: reply too long")
            .await {
            println!("Error: {}", err);
        }
    }

    async fn handle_message_update(&self, _message: Box<twilight_model::gateway::payload::incoming::MessageUpdate>, _client: &mut Client) {
        // TODO: handle message edits and stuff
    }
}
