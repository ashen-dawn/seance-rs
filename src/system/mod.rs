use std::{collections::HashMap, str::FromStr, time::Duration};

use tokio::{
    sync::mpsc::{channel, Sender},
    time::sleep,
};
use twilight_model::{
    channel::{
        Message,
    },
    id::{marker::UserMarker, Id},
};
use twilight_model::util::Timestamp;

use crate::config::{AutoproxyConfig, AutoproxyLatchScope, Member};

mod aggregator;
mod bot;
mod types;
use aggregator::MessageAggregator;
use bot::Bot;
pub use types::*;

use self::bot::MessageDuplicateError;

pub struct Manager {
    pub name: String,
    pub config: crate::config::System,
    pub bots: HashMap<MemberId, Bot>,
    pub latch_state: Option<(MemberId, Timestamp)>,
    pub system_sender: Option<Sender<SystemEvent>>,
}

impl Manager {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        Self {
            name: system_name,
            config: system_config,
            bots: HashMap::new(),
            latch_state: None,
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
            // Create gateway listener
            let mut bot = Bot::new(member_id, &member, reference_user_id);

            bot.set_message_handler(aggregator.get_sender());
            bot.set_system_handler(system_sender.clone());

            // Start gateway listener
            bot.start_listening();
            self.bots.insert(member_id, bot);
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
                let bot = if let Some((current_member, _)) = self.latch_state.clone() {
                    self.bots
                        .get(&current_member)
                        .expect(format!("No client for member {}", current_member).as_str())
                } else {
                    self.bots.iter().next().expect("No clients!").1
                };

                // We don't really care about the outcome here, we don't proxy afterwards
                let _ = bot.delete_message(message.channel_id, message.id).await;
                self.latch_state = None
            } else if message.content.starts_with(r"\\") {
                self.latch_state = None;
            }

            return;
        }

        // TODO: Non-latching prefixes maybe?

        // Check for prefix
        let match_prefix =
            self.config
                .members
                .iter()
                .enumerate()
                .find_map(|(member_id, member)| {
                    Some((member_id, member.matches_proxy_prefix(&message)?))
                });
        if let Some((member_id, matched_content)) = match_prefix {
            if let Ok(_) = self.proxy_message(&message, member_id, matched_content).await {
                self.latch_state = Some((member_id, timestamp));
                self.update_autoproxy_state_after_message(member_id, timestamp);
                self.update_status_of_system().await;
            }
            return
        }

        // Check for autoproxy
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
                    if let Some((member, last_timestamp)) = self.latch_state.clone() {
                        let time_since_last = timestamp.as_secs() - last_timestamp.as_secs();
                        if time_since_last <= (*timeout_seconds).into() {
                            if let Ok(_) = self.proxy_message(&message, member, message.content.as_str()).await {
                                self.latch_state = Some((member, timestamp));
                                self.update_autoproxy_state_after_message(member, timestamp);
                                self.update_status_of_system().await;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn proxy_message(&self, message: &Message, member: MemberId, content: &str) -> Result<(), ()> {
        let bot = self.bots.get(&member).expect("No client for member");

        let duplicate_result = bot.duplicate_message(message, content).await;

        if duplicate_result.is_err() {
            return Err(())
        }

        // Try to delete message first as that fails more often
        let delete_result = bot.delete_message(message.channel_id, message.id).await;

        if delete_result.is_err() {
            // Delete the duplicated message if that failed
            let _ = bot.delete_message(message.channel_id, duplicate_result.unwrap().id).await;
            return Err(())
        }

        Ok(())
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
        let bot = self.bots.get(&member).expect("No client for member");
        bot.set_status(status).await;
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

