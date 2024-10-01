use std::{collections::HashMap, str::FromStr, time::Duration};

use futures::future::join_all;
use tokio::{
    sync::mpsc::{channel, Sender},
    time::sleep,
};
use twilight_http::Client;
use twilight_model::http::attachment::Attachment;
use twilight_model::util::Timestamp;
use twilight_model::{
    channel::{
        message::{AllowedMentions, MentionType, MessageType},
        Message,
    },
    id::{marker::UserMarker, Id},
};

use crate::config::{AutoproxyConfig, AutoproxyLatchScope, Member};

mod aggregator;
mod gateway;
mod types;
use aggregator::MessageAggregator;
use gateway::Gateway;
pub use types::*;

pub struct Manager {
    pub name: String,
    pub config: crate::config::System,
    pub clients: HashMap<MemberId, Client>,
    pub gateways: HashMap<MemberId, Gateway>,
    pub latch_state: Option<(MemberId, Timestamp)>,
    pub last_presence: HashMap<MemberId, Status>,
    pub system_sender: Option<Sender<SystemEvent>>,
}

impl Manager {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        Self {
            name: system_name,
            config: system_config,
            clients: HashMap::new(),
            gateways: HashMap::new(),
            latch_state: None,
            last_presence: HashMap::new(),
            system_sender: None,
        }
    }

    pub fn find_member_by_name<'a>(
        &'a self,
        name: &String,
    ) -> Option<(MemberId, &'a crate::config::Member)> {
        self.config
            .members
            .iter()
            .enumerate()
            .find(|(_member_id, member)| member.name == *name)
    }

    pub fn find_member_by_id<'a>(&'a self, id: MemberId) -> Option<&'a Member> {
        self.config
            .members
            .iter()
            .enumerate()
            .find(|(member_id, _)| *member_id == id)
            .map_or(None, |(_member_id, member)| Some(member))
    }

    pub async fn start_clients(&mut self) {
        println!("Starting clients for system {}", self.name);

        let reference_user_id: Id<UserMarker> =
            Id::from_str(self.config.reference_user_id.as_str())
                .expect(format!("Invalid user ID: {}", self.config.reference_user_id).as_str());

        let (system_sender, mut system_receiver) = channel::<SystemEvent>(100);
        self.system_sender = Some(system_sender.clone());
        let mut aggregator = MessageAggregator::new();
        aggregator.set_handler(system_sender.clone());

        for (member_id, member) in self.config.members.iter().enumerate() {
            // Create outgoing client
            let client = twilight_http::Client::new(member.discord_token.clone());
            self.clients.insert(member_id, client);

            // Create gateway listener
            let mut listener = Gateway::new(member_id, &member, reference_user_id);

            listener.set_message_handler(aggregator.get_sender());
            listener.set_system_handler(system_sender.clone());

            // Start gateway listener
            listener.start_listening();
            self.gateways.insert(member_id, listener);
        }

        aggregator.start();

        let mut num_connected = 0;

        loop {
            match system_receiver.recv().await {
                Some(event) => match event {
                    SystemEvent::GatewayConnected(member_id) => {
                        let member = self
                            .find_member_by_id(member_id)
                            .expect("Could not find member");

                        num_connected += 1;
                        println!(
                            "Gateway client {} ({}) connected",
                            num_connected, member.name
                        );

                        if num_connected == self.config.members.len() {
                            let system_sender = system_sender.clone();
                            tokio::spawn(async move {
                                println!("All gateways connected");
                                sleep(Duration::from_secs(5)).await;
                                let _ = system_sender.send(SystemEvent::AllGatewaysConnected).await;
                            });
                        }
                    }
                    SystemEvent::GatewayClosed(member_id) => {
                        let member = self
                            .find_member_by_id(member_id)
                            .expect("Could not find member");

                        println!("Gateway client {} closed", member.name);

                        num_connected -= 1;
                    }
                    SystemEvent::NewMessage((event_time, message)) => {
                        self.handle_message(message, event_time).await;
                    }
                    SystemEvent::GatewayError(member_id, message) => {
                        let member = self
                            .find_member_by_id(member_id)
                            .expect("Could not find member");
                        println!("Gateway client {} ran into error {}", member.name, message);
                        return;
                    }
                    SystemEvent::AutoproxyTimeout(time_scheduled) => {
                        if let Some((_member, current_last_message)) = self.latch_state.clone() {
                            if current_last_message == time_scheduled {
                                println!("Autoproxy timeout has expired: {} (last sent), {} (timeout scheduled)", current_last_message.as_secs(), time_scheduled.as_secs());
                                self.latch_state = None;
                                self.update_status_of_system().await;
                            }
                        }
                    }
                    SystemEvent::AllGatewaysConnected => {
                        println!(
                            "Attempting to set startup status for system {}",
                            self.name.clone()
                        );
                        self.update_status_of_system().await;
                    }
                    _ => (),
                },
                None => return,
            }
        }
    }

    async fn handle_message(&mut self, message: Message, timestamp: Timestamp) {
        // TODO: Commands
        if message.content.eq("!panic") {
            panic!("Exiting due to user command");
        }

        // Escape sequence
        if message.content.starts_with(r"\") {
            if message.content == r"\\" {
                let client = if let Some((current_member, _)) = self.latch_state.clone() {
                    self.clients
                        .get(&current_member)
                        .expect(format!("No client for member {}", current_member).as_str())
                } else {
                    self.clients.iter().next().expect("No clients!").1
                };

                client
                    .delete_message(message.channel_id, message.id)
                    .await
                    .expect("Could not delete message");
                self.latch_state = None
            } else if message.content.starts_with(r"\\") {
                self.latch_state = None;
            }

            return;
        }

        // TODO: Non-latching prefixes maybe?

        // Check for prefix
        println!("Checking prefix");
        let match_prefix =
            self.config
                .members
                .iter()
                .enumerate()
                .find_map(|(member_id, member)| {
                    Some((member_id, member.matches_proxy_prefix(&message)?))
                });
        if let Some((member_id, matched_content)) = match_prefix {
            self.proxy_message(&message, member_id, matched_content)
                .await;
            println!("Updating proxy state to member id {}", member_id);
            self.update_autoproxy_state_after_message(member_id, timestamp);
            self.update_status_of_system().await;
            return;
        }

        // Check for autoproxy
        println!("Checking autoproxy");
        if let Some(autoproxy_config) = &self.config.autoproxy {
            match autoproxy_config {
                AutoproxyConfig::Member { name } => {
                    let (member_id, _member) = self
                        .find_member_by_name(&name)
                        .expect("Invalid autoproxy member name");
                    self.proxy_message(&message, member_id, message.content.as_str())
                        .await;
                }
                // TODO: Do something with the latch scope
                // TODO: Do something with presence setting
                AutoproxyConfig::Latch {
                    scope,
                    timeout_seconds,
                    presence_indicator,
                } => {
                    println!("Currently in latch mode");
                    if let Some((member, last_timestamp)) = self.latch_state.clone() {
                        println!("We have a latch state");
                        let time_since_last = timestamp.as_secs() - last_timestamp.as_secs();
                        println!("Time since last (seconds) {}", time_since_last);
                        if time_since_last <= (*timeout_seconds).into() {
                            println!("Proxying");
                            self.proxy_message(&message, member, message.content.as_str())
                                .await;
                            self.latch_state = Some((member, timestamp));
                            self.update_autoproxy_state_after_message(member, timestamp);
                            self.update_status_of_system().await;
                        }
                    }
                }
            }
        } else {
            println!("No autoproxy config?");
        }
    }

    async fn proxy_message(&self, message: &Message, member: MemberId, content: &str) {
        let client = self.clients.get(&member).expect("No client for member");

        if let Err(err) = self.duplicate_message(message, client, content).await {
            match err {
                MessageDuplicateError::MessageCreate(err) => {
                    if err.to_string().contains("Cannot send an empty message") {
                        client
                            .delete_message(message.channel_id, message.id)
                            .await
                            .expect("Could not delete message");
                    }
                }
                _ => println!("Error: {:?}", err),
            }
        } else {
            client
                .delete_message(message.channel_id, message.id)
                .await
                .expect("Could not delete message");
        }
    }

    async fn duplicate_message(
        &self,
        message: &Message,
        client: &Client,
        content: &str,
    ) -> Result<Message, MessageDuplicateError> {
        let mut create_message = client.create_message(message.channel_id).content(content)?;

        let mut allowed_mentions = AllowedMentions {
            parse: Vec::new(),
            replied_user: false,
            roles: message.mention_roles.clone(),
            users: message.mentions.iter().map(|user| user.id).collect(),
        };

        if message.mention_everyone {
            allowed_mentions.parse.push(MentionType::Everyone);
        }

        if message.kind == MessageType::Reply {
            if let Some(ref_message) = message.referenced_message.as_ref() {
                create_message = create_message.reply(ref_message.id);

                let pings_referenced_author = message
                    .mentions
                    .iter()
                    .any(|user| user.id == ref_message.author.id);

                if pings_referenced_author {
                    allowed_mentions.replied_user = true;
                } else {
                    allowed_mentions.replied_user = false;
                }
            } else {
                panic!("Cannot proxy message: Was reply but no referenced message");
            }
        }

        let attachments = join_all(message.attachments.iter().map(|attachment| async {
            let filename = attachment.filename.clone();
            let description_opt = attachment.description.clone();
            let bytes = reqwest::get(attachment.proxy_url.clone())
                .await?
                .bytes()
                .await?;
            let mut new_attachment =
                Attachment::from_bytes(filename, bytes.try_into().unwrap(), attachment.id.into());

            if let Some(description) = description_opt {
                new_attachment.description(description);
            }

            Ok(new_attachment)
        }))
        .await
        .iter()
        .filter_map(
            |result: &Result<Attachment, MessageDuplicateError>| match result {
                Ok(attachment) => Some(attachment.clone()),
                Err(_) => None,
            },
        )
        .collect::<Vec<_>>();

        if attachments.len() > 0 {
            create_message = create_message.attachments(attachments.as_slice())?;
        }

        if let Some(flags) = message.flags {
            create_message = create_message.flags(flags);
        }

        create_message = create_message.allowed_mentions(Some(&allowed_mentions));
        let new_message = create_message.await?.model().await?;

        Ok(new_message)
    }

    fn update_autoproxy_state_after_message(&mut self, member: MemberId, timestamp: Timestamp) {
        match &self.config.autoproxy {
            None => (),
            Some(AutoproxyConfig::Member { name: _ }) => (),
            Some(AutoproxyConfig::Latch {
                scope,
                timeout_seconds,
                presence_indicator: _,
            }) => {
                self.latch_state = Some((member, timestamp));

                if let Some(channel) = self.system_sender.clone() {
                    let last_message = timestamp.clone();
                    let timeout_seconds = timeout_seconds.clone();

                    tokio::spawn(async move {
                        sleep(Duration::from_secs(timeout_seconds.into())).await;
                        channel
                            .send(SystemEvent::AutoproxyTimeout(last_message))
                            .await
                            .expect("Channel has closed");
                    });
                }
            }
        }
    }

    async fn update_status_of_system(&mut self) {
        let member_states: Vec<(MemberId, Status)> = self
            .config
            .members
            .iter()
            .enumerate()
            .map(|(member_id, member)| {
                (
                    member_id,
                    match &self.config.autoproxy {
                        None => Status::Invisible,
                        Some(AutoproxyConfig::Member { name }) => {
                            if member.name == *name {
                                Status::Online
                            } else {
                                Status::Invisible
                            }
                        }
                        Some(AutoproxyConfig::Latch {
                            scope,
                            timeout_seconds: _,
                            presence_indicator,
                        }) => {
                            if let AutoproxyLatchScope::Server = scope {
                                Status::Invisible
                            } else if !presence_indicator {
                                Status::Invisible
                            } else {
                                match &self.latch_state {
                                    Some((latch_member, _last_timestamp)) => {
                                        if member_id == *latch_member {
                                            Status::Online
                                        } else {
                                            Status::Invisible
                                        }
                                    }
                                    None => Status::Invisible,
                                }
                            }
                        }
                    },
                )
            })
            .collect();

        for (member, status) in member_states {
            self.update_status_of_member(member, status).await;
        }
    }

    async fn update_status_of_member(&mut self, member: MemberId, status: Status) {
        let last_status = *self.last_presence.get(&member).unwrap_or(&Status::Offline);

        if status == last_status {
            return;
        }

        if let Some(gateway) = self.gateways.get(&member) {
            gateway.set_status(status).await;

            self.last_presence.insert(member, status);
        } else {
            let full_member = self
                .find_member_by_id(member)
                .expect("Cannot look up member");
            println!(
                "Could not look up gateway for member ID {} ({})",
                member, full_member.name
            );
        }
    }
}

impl crate::config::Member {
    pub fn matches_proxy_prefix<'a>(&self, message: &'a Message) -> Option<&'a str> {
        match self.message_pattern.captures(message.content.as_str()) {
            None => None,
            Some(captures) => match captures.name("content") {
                None => None,
                Some(matched_content) => Some(matched_content.as_str()),
            },
        }
    }
}

#[derive(Debug)]
enum MessageDuplicateError {
    MessageValidation(twilight_validate::message::MessageValidationError),
    AttachmentRequest(reqwest::Error),
    MessageCreate(twilight_http::error::Error),
    ResponseDeserialization(twilight_http::response::DeserializeBodyError),
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
