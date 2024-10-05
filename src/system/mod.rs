use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};

use tokio::{
    sync::{mpsc::{channel, Sender}, RwLock},
    time::sleep,
};
use twilight_model::id::{marker::UserMarker, Id};
use twilight_model::util::Timestamp;

use crate::config::{AutoproxyConfig, AutoproxyLatchScope, Member};

mod aggregator;
mod bot;
mod types;
use aggregator::MessageAggregator;
use bot::Bot;
pub use types::*;


pub struct Manager {
    pub name: String,
    pub config: crate::config::System,
    pub bots: HashMap<MemberId, Bot>,
    pub latch_state: Option<(MemberId, Timestamp)>,
    pub system_sender: Option<Sender<SystemEvent>>,
    pub aggregator: MessageAggregator,
    pub reference_user_id: UserId,
}

impl Manager {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        Self {
            reference_user_id: Id::from_str(&system_config.reference_user_id.as_str())
                .expect(format!("Invalid user id for system {}", &system_name).as_str()),
            name: system_name,
            config: system_config,
            bots: HashMap::new(),
            latch_state: None,
            system_sender: None,
            aggregator: MessageAggregator::new(),
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

        let (system_sender, mut system_receiver) = channel::<SystemEvent>(100);
        self.system_sender = Some(system_sender.clone());
        self.aggregator.set_system_handler(system_sender.clone()).await;
        self.aggregator.start();

        for member_id in 0..self.config.members.len() {
            self.start_bot(member_id).await;
        }

        loop {
            match system_receiver.recv().await {
                Some(SystemEvent::GatewayConnected(member_id)) => {
                    let member = self.find_member_by_id(member_id).unwrap();

                    println!("Gateway client {} ({}) connected", member.name, member_id);
                }

                Some(SystemEvent::GatewayError(member_id, message)) => {
                    let member = self.find_member_by_id(member_id).unwrap();

                    println!("Gateway client {} ran into error {}", member.name, message);
                }

                Some(SystemEvent::GatewayClosed(member_id)) => {
                    let member = self.find_member_by_id(member_id).unwrap();

                    println!("Gateway client {} closed", member.name);

                    self.start_bot(member_id).await;
                }

                Some(SystemEvent::NewMessage(event_time, message)) => {
                    self.handle_message(message, event_time).await;
                }

                Some(SystemEvent::RefetchMessage(member_id, message_id, channel_id)) => {
                    let bot = self.bots.get(&member_id).unwrap();
                    bot.refetch_message(message_id, channel_id).await;
                }

                Some(SystemEvent::AutoproxyTimeout(time_scheduled)) => {
                    if let Some((_member, current_last_message)) = self.latch_state.clone() {
                        if current_last_message == time_scheduled {
                            println!("Autoproxy timeout has expired: {} (last sent), {} (timeout scheduled)", current_last_message.as_secs(), time_scheduled.as_secs());
                            self.latch_state = None;
                            self.update_status_of_system().await;
                        }
                    }
                },

                Some(SystemEvent::UpdateClientStatus(member_id)) => {
                    let bot = self.bots.get(&member_id).unwrap();

                    // TODO: handle other presence modes
                    if let Some((latched_id, _)) = self.latch_state {
                        if latched_id == member_id {
                            bot.set_status(Status::Online).await;
                            continue
                        }
                    }

                    bot.set_status(Status::Invisible).await;
                }

                _ => continue,
            }
        }
    }

    async fn start_bot(&mut self, member_id: MemberId) {
        let member = self.find_member_by_id(member_id).unwrap();

        // Create gateway listener
        let mut bot = Bot::new(member_id, &member, self.reference_user_id);

        bot.set_message_handler(self.aggregator.get_sender().await).await;
        bot.set_system_handler(self.system_sender.as_ref().unwrap().clone()).await;

        // Start gateway listener
        bot.start();
        self.bots.insert(member_id, bot);

        // Schedule status update after a few seconds
        let rx = self.system_sender.as_ref().unwrap().clone();
        tokio::spawn(async move {
            sleep(Duration::from_secs(10)).await;
            let _ = rx.send(SystemEvent::UpdateClientStatus(member_id)).await;
        });
    }

    async fn handle_message(&mut self, message: TwiMessage, timestamp: Timestamp) {
        // TODO: Commands
        if message.content.eq("!panic") {
           self.bots.iter_mut().next().unwrap().1.shutdown().await;
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

    async fn proxy_message(&self, message: &TwiMessage, member: MemberId, content: &str) -> Result<(), ()> {
        let bot = self.bots.get(&member).expect("No client for member");

        let duplicate_result = bot.duplicate_message(message, content).await;

        if duplicate_result.is_err() {
            println!("Could not copy message: {:?}", duplicate_result);
            return Err(())
        }

        // Try to delete message first as that fails more often
        let delete_result = bot.delete_message(message.channel_id, message.id).await;

        if delete_result.is_err() {
            println!("Could not delete message: {:?}", delete_result);

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
    pub fn matches_proxy_prefix<'a>(&self, message: &'a TwiMessage) -> Option<&'a str> {
        match self.message_pattern.captures(message.content.as_str()) {
            None => None,
            Some(captures) => match captures.name("content") {
                None => None,
                Some(matched_content) => Some(matched_content.as_str()),
            },
        }
    }
}

