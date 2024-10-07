use std::{collections::HashMap, num::NonZeroUsize, str::FromStr, time::Duration};

use lru::LruCache;
use tokio::{
    sync::mpsc::{channel, Sender},
    time::sleep,
};
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::{channel::message::{MessageReference, MessageType, ReactionType}, id::{marker::UserMarker, Id}};
use twilight_model::util::Timestamp;

use crate::config::{AutoproxyConfig, AutoproxyLatchScope, Member};

mod aggregator;
mod bot;
mod types;
mod message_parser;

use message_parser::MessageParser;
use aggregator::MessageAggregator;
use bot::Bot;
pub use types::*;

use self::message_parser::{Command, ParsedMessage};


pub struct Manager {
    pub name: String,
    pub config: crate::config::System,
    pub bots: HashMap<MemberId, Bot>,
    pub latch_state: Option<(MemberId, Timestamp)>,
    pub system_sender: Option<Sender<SystemEvent>>,
    pub aggregator: MessageAggregator,
    pub send_cache: LruCache<ChannelId, TwiMessage>,
    pub reference_user_id: UserId,
}

impl Manager {
    pub fn new(system_name: String, system_config: crate::config::System) -> Self {
        Self {
            reference_user_id: Id::from_str(&system_config.reference_user_id.as_str())
                .expect(format!("Invalid user id for system {}", &system_name).as_str()),
            aggregator: MessageAggregator::new(system_config.members.len()),
            name: system_name,
            config: system_config,
            bots: HashMap::new(),
            latch_state: None,
            system_sender: None,
            send_cache: LruCache::new(NonZeroUsize::new(15).unwrap()),
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
                Some(SystemEvent::GatewayConnected(member_id, user_id)) => {
                    self.config.members.iter_mut().enumerate()
                        .find(|(id, _)| *id == member_id).unwrap().1.user_id = Some(user_id);

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

                Some(SystemEvent::NewMessage(event_time, message, member_id)) => {
                    self.handle_message(message, event_time, member_id).await;
                }

                Some(SystemEvent::RefetchMessage(member_id, message_id, channel_id)) => {
                    let bot = self.bots.get(&member_id).unwrap();
                    bot.resend_message(message_id, channel_id).await;
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

    async fn handle_message(&mut self, message: TwiMessage, timestamp: Timestamp, seen_by: MemberId) {
        let bot = self.bots.get(&seen_by).expect("No client for member");

        // If message type is reply, use that
        let referenced_message = if let MessageType::Reply = message.kind {
            message.referenced_message.as_ref().map(|message| message.as_ref())
        } else {
            // Otherwise, check cache for lest message sent in channel
            if self.send_cache.contains(&message.channel_id) {
                self.send_cache.get(&message.channel_id)
            } else {
                // Or look it up if it's not in cache
                let system_bot_ids : Vec<UserId> = self.config.members.iter().filter_map(|m| m.user_id).collect();
                let recent_messages = bot.fetch_recent_channel_messages(message.channel_id).await;

                let last_in_channel = recent_messages.map(|messages| {
                    messages.into_iter().filter(|message|
                        system_bot_ids.contains(&message.author.id)
                    ).max_by_key(|message| message.timestamp.as_micros())
                }).ok().flatten();

                // Since we did all this work to look it up, insert it into cache
                if let Some(last) = last_in_channel {
                    self.send_cache.put(message.channel_id, last);
                } else {
                    println!("WARNING: Could not look up most recent message in channel {}", message.channel_id);
                };

                // Return the message referenced from cache so there's no unnecessary clone
                self.send_cache.get(&message.channel_id)
            }
        };

        let parsed_message = MessageParser::parse(&message, referenced_message, &self.config, self.latch_state);

        match parsed_message {
            message_parser::ParsedMessage::UnproxiedMessage => (),

            message_parser::ParsedMessage::LatchClear(member_id) => {
                let _ = self.bots.get(&member_id).unwrap().delete_message(message.channel_id, message.id).await;
                self.latch_state = None;
                self.update_status_of_system().await;
            },

            message_parser::ParsedMessage::SetProxyAndDelete(member_id) => {
                let _ = self.bots.get(&member_id).unwrap().delete_message(message.channel_id, message.id).await;
                self.update_autoproxy_state_after_message(member_id, message.timestamp);
                self.update_status_of_system().await;
            }

            message_parser::ParsedMessage::ProxiedMessage { member_id, message_content, latch } => {
                if let Ok(_) = self.proxy_message(&message, member_id, message_content.as_str()).await {
                    if latch {
                        self.update_autoproxy_state_after_message(member_id, timestamp);
                        self.update_status_of_system().await;
                    }
                }
            },

            message_parser::ParsedMessage::Command(Command::Edit(member_id, message_id, new_content)) => {
                let bot = self.bots.get(&member_id).unwrap();

                let author = MessageParser::get_member_id_from_user_id(referenced_message.unwrap().author.id, &self.config);
                if author.is_none() {
                    println!("Cannot edit another user's message");
                    let _ = self.bots.get(&member_id).unwrap().react_message(message.channel_id, message.id, &RequestReactionType::Unicode { name: "🛑" }).await;
                    return
                }

                if let Ok(new_message) = bot.edit_message(message.channel_id, message_id, new_content).await {

                    // If we just edited the most recently sent message in this channel, update
                    // cache for future edit commands
                    if self.send_cache.get(&new_message.channel_id).map_or(MessageId::new(1u64), |m| m.id) == message_id {
                        self.send_cache.put(new_message.channel_id, new_message);
                    }

                    // Delete the command message
                    let _ = bot.delete_message(message.channel_id, message.id).await;
                }
            }

            message_parser::ParsedMessage::Command(Command::Reproxy(member_id, message_id)) => {
                if !referenced_message.map(|message| message.id == message_id).unwrap_or(false) {
                    println!("ERROR: Attempted reproxy on message other than referenced_message");
                    let _ = self.bots.get(&member_id).unwrap().react_message(message.channel_id, message.id, &RequestReactionType::Unicode { name: "⁉️" }).await;
                    return
                }

                let author = MessageParser::get_member_id_from_user_id(referenced_message.unwrap().author.id, &self.config);
                if author.is_none() {
                    println!("Cannot reproxy another user's message");
                    let _ = self.bots.get(&member_id).unwrap().react_message(message.channel_id, message.id, &RequestReactionType::Unicode { name: "🛑" }).await;
                    return
                }

                if author.unwrap() != member_id {
                    // TODO: Don't allow this if other messages have been sent maybe?
                    let orig = referenced_message.unwrap().clone();
                    if let Ok(_) = self.proxy_message(&orig, member_id, orig.content.as_str()).await {
                        self.update_autoproxy_state_after_message(member_id, timestamp);
                        self.update_status_of_system().await;
                    }
                } else {
                    println!("Not reproxying under same user");
                }

                let bot = self.bots.get(&member_id).unwrap();
                let _ = bot.delete_message(message.channel_id, message.id).await;
            }

            message_parser::ParsedMessage::Command(Command::Delete(message_id)) => {
                let member_id = self.latch_state.map(|(id,_)| id).unwrap_or(0);

                let author = MessageParser::get_member_id_from_user_id(referenced_message.unwrap().author.id, &self.config);
                if author.is_none() {
                    println!("Cannot delete another user's message");
                    let _ = self.bots.get(&member_id).unwrap().react_message(message.channel_id, message.id, &RequestReactionType::Unicode { name: "🛑" }).await;
                    return
                }

                let bot = self.bots.get(&member_id).unwrap();
                let _ = bot.delete_message(message.channel_id, message_id).await;
                let _ = bot.delete_message(message.channel_id, message.id).await;
            }

            message_parser::ParsedMessage::Command(Command::UnknownCommand) => {
                let member_id = if let Some((member_id, _)) = self.latch_state {
                    member_id
                } else {
                    0
                };

                let _ = self.bots.get(&member_id).unwrap().react_message(message.channel_id, message.id, &RequestReactionType::Unicode { name: "⁉️" }).await;
            },
            message_parser::ParsedMessage::Command(_) => todo!(),
            message_parser::ParsedMessage::EmoteAdd(_, _, _) => todo!(),
            message_parser::ParsedMessage::EmoteRemove(_, _, _) => todo!(),
        }
    }

    async fn proxy_message(&mut self, message: &TwiMessage, member: MemberId, content: &str) -> Result<(), ()> {
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

        // Sent successfully, add to send cache
        let sent_message = duplicate_result.unwrap();
        self.send_cache.put(sent_message.channel_id, sent_message);

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

