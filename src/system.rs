use std::{collections::HashMap, num::NonZeroUsize, str::FromStr};

use tokio::sync::mpsc::channel;
use twilight_http::Client;
use twilight_model::id::{Id, marker::{MessageMarker, UserMarker}};
use twilight_model::util::Timestamp;

use crate::listener::{Listener, ClientEvent};

pub struct System {
    pub name: String,
    pub config: crate::config::System,
    pub message_dedup_cache: lru::LruCache<Id<MessageMarker>, Timestamp>,
    pub clients: HashMap<String, Client>,
}

impl System {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        System {
            name: system_name,
            config: system_config,
            message_dedup_cache: lru::LruCache::new(NonZeroUsize::new(100).unwrap()),
            clients: HashMap::new(),
        }
    }

    pub async fn start_clients(&mut self) {
        println!("Starting clients for system {}", self.name);

        let (tx, mut rx) = channel::<ClientEvent>(100);

        let reference_user_id : Id<UserMarker> = Id::from_str(self.config.reference_user_id.as_str())
            .expect(format!("Invalid user ID: {}", self.config.reference_user_id).as_str());

        for member in self.config.members.iter() {
            let client = twilight_http::Client::new(member.discord_token.clone());
            self.clients.insert(member.name.clone(), client);

            let tx = tx.clone();
            let member = member.clone();
            tokio::spawn(async move {
                let mut listener = Listener::new(member, reference_user_id);
                listener.start_listening(tx).await;
            });
        }

        loop {
            match rx.recv().await {
                Some(event) => match event {
                    ClientEvent::Message { event_time, message_id, content, author: _ } => {
                        if self.is_new_message(message_id, event_time) {
                            self.handle_message(content).await;
                        }
                    }
                    ClientEvent::Error(_err) => {
                        println!("Client ran into an error for system {}", self.name);
                        return
                    },
                },
                None => {
                    return
                },
            }
        }
    }

    fn is_new_message(&mut self, message_id: Id<MessageMarker>, timestamp: Timestamp) -> bool {
        let last_seen_timestamp = self.message_dedup_cache.get(&message_id);
        let current_timestamp = timestamp;

        if last_seen_timestamp.is_none() || last_seen_timestamp.unwrap().as_micros() < current_timestamp.as_micros() {
            self.message_dedup_cache.put(message_id, timestamp);
            true
        } else {
            false
        }
    }

    async fn handle_message(&mut self, content: String) {
        println!("Message: {}", content);
    }
}
