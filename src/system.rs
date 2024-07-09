use std::{collections::HashMap, num::NonZeroUsize, str::FromStr};

use tokio::sync::mpsc::channel;
use twilight_http::Client;
use twilight_model::id::{Id, marker::{ChannelMarker, MessageMarker, UserMarker}};
use twilight_model::util::Timestamp;

use crate::{config::{AutoproxyConfig, Member, MemberName}, listener::{Listener, ClientEvent}};

pub struct System {
    pub name: String,
    pub config: crate::config::System,
    pub message_dedup_cache: lru::LruCache<Id<MessageMarker>, Timestamp>,
    pub clients: HashMap<MemberName, Client>,
    pub latch_state: Option<(Member, Timestamp)>,
}

impl System {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        System {
            name: system_name,
            config: system_config,
            message_dedup_cache: lru::LruCache::new(NonZeroUsize::new(100).unwrap()),
            clients: HashMap::new(),
            latch_state: None,
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
                    ClientEvent::Message { event_time, message_id, channel_id, content, author: _ } => {
                        if self.is_new_message(message_id, event_time) {
                            self.handle_message(message_id, channel_id, content, event_time).await;
                        }
                    },
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

    async fn handle_message(&mut self, message_id: Id<MessageMarker>, channel_id: Id<ChannelMarker>, content: String, timestamp: Timestamp) {
        // Check for command
        // TODO: Commands
        // TODO: Escaping

        // TODO: Non-latching prefixes maybe?
        
        // Check for prefix
        let match_prefix = self.config.members.iter().find_map(|member| Some((member, member.matches_proxy_prefix(&content)?)));
        if let Some((member, matched_content)) = match_prefix {
            self.proxy_message(message_id, channel_id, member, matched_content).await;
            self.update_autoproxy_state_after_message(member.clone(), timestamp);
            return
        }


        // Check for autoproxy
        if let Some(autoproxy_config) = &self.config.autoproxy {
            match autoproxy_config {
                AutoproxyConfig::Member {name} => {
                    let member = self.config.members.iter().find(|member| member.name == *name).expect("Invalid autoproxy member name");
                    self.proxy_message(message_id, channel_id, member, content.as_str()).await;
                },
                // TODO: Do something with the latch scope
                // TODO: Do something with presence setting
                AutoproxyConfig::Latch { scope, timeout_seconds, presence_indicator } => {
                    if let Some((member, last_timestamp)) = &self.latch_state {
                        let time_since_last = timestamp.as_secs() - last_timestamp.as_secs();
                        if time_since_last <= (*timeout_seconds).into() {
                            self.proxy_message(message_id, channel_id, &member, content.as_str()).await;
                            self.latch_state = Some((member.clone(), timestamp));
                        }
                    }
                },
            }
        }
    }

    async fn proxy_message(&self, message_id: Id<MessageMarker>, channel_id: Id<ChannelMarker>, member: &Member, content: &str) {
        let client = self.clients.get(&member.name).expect("No client for member");

        if let Ok(_) = client.create_message(channel_id)
            .content(content).expect("Cannot set content").await {
                client.delete_message(channel_id, message_id).await.expect("Could not delete message");
            }
    }

    fn update_autoproxy_state_after_message(&mut self, member: Member, timestamp: Timestamp) {
        match &self.config.autoproxy {
            None => (),
            Some(AutoproxyConfig::Member { name }) => (),
            Some(AutoproxyConfig::Latch { scope, timeout_seconds, presence_indicator }) => {
                self.latch_state = Some((member.clone(), timestamp));
            }
        }
    }
}



impl crate::config::Member {
    pub fn matches_proxy_prefix<'a>(&self, content: &'a String) -> Option<&'a str> {
        match self.message_pattern.captures(content.as_str()) {
            None => None,
            Some(captures) => {
                let full_match = captures.get(0).unwrap();

                if full_match.len() != content.len() {
                    return None
                }

                match captures.name("content") {
                    None => None,
                    Some(matched_content) => Some(matched_content.as_str()),
                }
            }
        }
    }
}
