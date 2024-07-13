use std::{collections::HashMap, num::NonZeroUsize, str::FromStr, time::Duration};

use tokio::{sync::mpsc::{channel, Receiver, Sender}, time::{Sleep,sleep}};
use futures::future::join_all;
use twilight_gateway::MessageSender;
use twilight_http::Client;
use twilight_model::{channel::{message::MessageType, Message}, gateway::{payload::outgoing::{update_presence::UpdatePresencePayload, UpdatePresence}, presence::Status, OpCode}, id::{Id, marker::{ChannelMarker, MessageMarker, UserMarker}}};
use twilight_model::util::Timestamp;
use twilight_model::http::attachment::Attachment;

use crate::{config::{AutoproxyConfig, Member, MemberName}, listener::{Listener, ClientEvent}};

pub struct System {
    pub name: String,
    pub config: crate::config::System,
    pub message_dedup_cache: lru::LruCache<Id<MessageMarker>, Timestamp>,
    pub clients: HashMap<MemberName, Client>,
    pub latch_state: Option<(Member, Timestamp)>,
    pub channel: (Sender<ClientEvent>, Receiver<ClientEvent>),
    pub gateway_channels: HashMap<MemberName, MessageSender>,
    pub last_presence: HashMap<MemberName, Status>,
}

impl System {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        System {
            name: system_name,
            config: system_config,
            message_dedup_cache: lru::LruCache::new(NonZeroUsize::new(100).unwrap()),
            clients: HashMap::new(),
            latch_state: None,
            channel: channel::<ClientEvent>(100),
            gateway_channels: HashMap::new(),
            last_presence: HashMap::new(),
        }
    }

    pub async fn start_clients(&mut self) {
        println!("Starting clients for system {}", self.name);

        let reference_user_id : Id<UserMarker> = Id::from_str(self.config.reference_user_id.as_str())
            .expect(format!("Invalid user ID: {}", self.config.reference_user_id).as_str());

        for member in self.config.members.iter() {
            let client = twilight_http::Client::new(member.discord_token.clone());
            self.clients.insert(member.name.clone(), client);

            let tx = self.channel.0.clone();
            let member = member.clone();
            tokio::spawn(async move {
                let mut listener = Listener::new(member, reference_user_id);
                listener.start_listening(tx).await;
            });
        }

        loop {
            match self.channel.1.recv().await {
                Some(event) => match event {
                    ClientEvent::Ready { client_name, send_channel } => {
                        self.gateway_channels.insert(client_name, send_channel);
                    },
                    ClientEvent::Message { event_time, message } => {
                        if self.is_new_message(message.id, event_time) {
                            self.handle_message(message, event_time).await;
                        }
                    },
                    ClientEvent::Error(_err) => {
                        println!("Client ran into an error for system {}", self.name);
                        return
                    },
                    ClientEvent::AutoproxyTimeout { last_message } => {
                        if let Some((_member, current_last_message)) = self.latch_state.clone() {
                            if current_last_message == last_message {
                                println!("Autoproxy timeout has expired: {} (last sent), {} (timeout scheduled)", current_last_message.as_secs(), last_message.as_secs());
                                self.latch_state = None;
                                self.update_status_of_system();
                            } else {
                                println!("Autoproxy timeout called but not expired: {} (last sent), {} (timeout scheduled)", current_last_message.as_secs(), last_message.as_secs());
                            }
                        }
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

    async fn handle_message(&mut self, message: Message, timestamp: Timestamp) {
        // TODO: Commands

        // Escape sequence
        if message.content.starts_with(r"\") {
            if message.content == r"\\" {
                let client = if let Some((current_member, _)) = self.latch_state.clone() {
                    self.clients.get(&current_member.name).expect(format!("No client for member {}", current_member.name).as_str())
                } else {
                    self.clients.iter().next().expect("No clients!").1
                };

                client.delete_message(message.channel_id, message.id).await.expect("Could not delete message");
                self.latch_state = None
            } else if message.content.starts_with(r"\\") {
                self.latch_state = None;
            }

            return
        }

        // TODO: Non-latching prefixes maybe?
        
        // Check for prefix
        let match_prefix = self.config.members.iter().find_map(|member| Some((member, member.matches_proxy_prefix(&message)?)));
        if let Some((member, matched_content)) = match_prefix {
            self.proxy_message(&message, member, matched_content).await;
            self.update_autoproxy_state_after_message(member.clone(), timestamp);
            self.update_status_of_system();
            return
        }

        // Check for autoproxy
        if let Some(autoproxy_config) = &self.config.autoproxy {
            match autoproxy_config {
                AutoproxyConfig::Member {name} => {
                    let member = self.config.members.iter().find(|member| member.name == *name).expect("Invalid autoproxy member name");
                    self.proxy_message(&message, member, message.content.as_str()).await;
                },
                // TODO: Do something with the latch scope
                // TODO: Do something with presence setting
                AutoproxyConfig::Latch { scope, timeout_seconds, presence_indicator } => {
                    if let Some((member, last_timestamp)) = self.latch_state.clone() {
                        let time_since_last = timestamp.as_secs() - last_timestamp.as_secs();
                        if time_since_last <= (*timeout_seconds).into() {
                            self.proxy_message(&message, &member, message.content.as_str()).await;
                            self.latch_state = Some((member.clone(), timestamp));
                            self.update_autoproxy_state_after_message(member.clone(), timestamp);
                            self.update_status_of_system();
                        }
                    }
                },
            }
        }
    }

    async fn proxy_message(&self, message: &Message, member: &Member, content: &str) {
        let client = self.clients.get(&member.name).expect("No client for member");

        if let Err(err) = self.duplicate_message(message, client, content).await {
            match err {
                MessageDuplicateError::MessageCreate(err) => {
                    if err.to_string().contains("Cannot send an empty message") {
                        client.delete_message(message.channel_id, message.id).await.expect("Could not delete message");
                    }
                },
                _ => println!("Error: {:?}", err),
            }
        } else {
            client.delete_message(message.channel_id, message.id).await.expect("Could not delete message");
        }
    }

    async fn duplicate_message(&self, message: &Message, client: &Client, content: &str) -> Result<Message, MessageDuplicateError> {
        let mut create_message = client.create_message(message.channel_id)
            .content(content)?;

        if message.kind == MessageType::Reply {
            create_message = create_message.reply(
                message.referenced_message.as_ref().expect("Message was reply but no referenced message").id
            );
        }

        let attachments = join_all(message.attachments.iter().map(|attachment| async {
            let filename = attachment.filename.clone();
            let description = attachment.description.clone();
            let bytes = reqwest::get(attachment.proxy_url.clone()).await?.bytes().await?;

            // TODO: keep description
            Ok(Attachment::from_bytes(filename, bytes.try_into().unwrap(), attachment.id.into()))
        })).await.iter().filter_map(|result: &Result<Attachment, MessageDuplicateError>| match result {
            Ok(attachment) => Some(attachment.clone()),
            Err(_) => None,
        }).collect::<Vec<_>>();

        if attachments.len() > 0 {
            create_message = create_message.attachments(attachments.as_slice())?;
        }

        let new_message = create_message.await?.model().await?;

        Ok(new_message)
    }

    fn update_autoproxy_state_after_message(&mut self, member: Member, timestamp: Timestamp) {
        match &self.config.autoproxy {
            None => (),
            Some(AutoproxyConfig::Member { name }) => (),
            Some(AutoproxyConfig::Latch { scope, timeout_seconds, presence_indicator }) => {
                self.latch_state = Some((member.clone(), timestamp));

                let tx = self.channel.0.clone();
                let last_message = timestamp.clone();
                let timeout_seconds = timeout_seconds.clone();

                tokio::spawn(async move {
                    sleep(Duration::from_secs(timeout_seconds.into())).await;
                    tx.send(ClientEvent::AutoproxyTimeout { last_message })
                        .await.expect("Channel has closed");
                });
            }
        }
    }

    fn update_status_of_system(&mut self) {
        let member_states : Vec<(Member, Status)> = self.config.members.iter().map(|member| {
            match &self.config.autoproxy {
                None => (member.clone(), Status::Invisible),
                Some(AutoproxyConfig::Member { name }) => (member.clone(), if member.name == *name {
                    Status::Online
                } else {
                    Status::Invisible
                }),
                Some(AutoproxyConfig::Latch { scope, timeout_seconds, presence_indicator }) => 
                    match &self.latch_state {
                        Some((latch_member, _last_timestamp)) => (member.clone(), if member.name == latch_member.name {
                            Status::Online
                        } else {
                            Status::Invisible
                        }),
                        None => (member.clone(), Status::Invisible),
                    }
            }
        }).collect();

        for (member, status) in member_states {
            self.update_status_of_member(&member, status);
        }
    }

    fn update_status_of_member(&mut self, member: &Member, status: Status) {
        let last_status = *self.last_presence.get(&member.name).unwrap_or(&Status::Offline);

        if status == last_status {
            return
        }

        let gateway_channel = self.gateway_channels.get(&member.name).expect("No gateway shard for member");
        gateway_channel.command(&UpdatePresence {
            d: UpdatePresencePayload {
                activities: Vec::new(),
                afk: false,
                since: None,
                status,
            },
            op: OpCode::PresenceUpdate,
        }).expect("Could not send command to gateway");

        self.last_presence.insert(member.name.clone(), status);
    }
}



impl crate::config::Member {
    pub fn matches_proxy_prefix<'a>(&self, message: &'a Message) -> Option<&'a str> {
        match self.message_pattern.captures(message.content.as_str()) {
            None => None,
            Some(captures) => {
                let full_match = captures.get(0).unwrap();

                if full_match.len() != message.content.len() {
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

#[derive(Debug)]
enum MessageDuplicateError {
    MessageValidation(twilight_validate::message::MessageValidationError),
    AttachmentRequest(reqwest::Error),
    MessageCreate(twilight_http::error::Error),
    ResponseDeserialization(twilight_http::response::DeserializeBodyError)
}

impl From<twilight_validate::message::MessageValidationError> for MessageDuplicateError {
    fn from(value: twilight_validate::message::MessageValidationError) -> Self {
        MessageDuplicateError::MessageValidation(value)
    }
}

impl From<reqwest::Error> for MessageDuplicateError {
    fn from(value: reqwest::Error) -> Self {
        MessageDuplicateError::AttachmentRequest(value)
    }
}

impl From<twilight_http::error::Error> for MessageDuplicateError {
    fn from(value: twilight_http::error::Error) -> Self {
        MessageDuplicateError::MessageCreate(value)
    }
}

impl From<twilight_http::response::DeserializeBodyError> for MessageDuplicateError {
    fn from(value: twilight_http::response::DeserializeBodyError) -> Self {
        MessageDuplicateError::ResponseDeserialization(value)
    }
}
