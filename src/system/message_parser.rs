use std::sync::LazyLock;
use regex::{Regex, RegexBuilder};

use crate::config::System;

use twilight_mention::ParseMention;
use twilight_model::id::{marker::UserMarker, Id};
use super::{FullMessage, MemberId, MessageId, Timestamp, UserId};

pub enum ParsedMessage {
    Command(Command),
    SetProxyAndDelete(MemberId),
    ProxiedMessage {
        member_id: MemberId,
        message_content: String,
        latch: bool,
    },
    UnproxiedMessage,
    LatchClear(MemberId),

    // TODO: Figure out how to represent emotes
    EmoteAdd(MemberId, MessageId, ()),
    EmoteRemove(MemberId, MessageId, ()),
}

pub enum Command {
    Edit(MemberId, MessageId, String),
    Reproxy(MemberId, MessageId),
    Nick(MemberId, String),
    ReloadSystemConfig,
    ExitSéance,
    UnknownCommand,
    InvalidCommand
}

pub struct MessageParser {}

static CORRECTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
   Regex::new(r"^\*\B+$").unwrap()
});

impl MessageParser {
    pub fn parse(message: &FullMessage, secondary_message: Option<&FullMessage>, system_config: &System, latch_state: Option<(MemberId, Timestamp)>) -> ParsedMessage {
        if message.content == r"\\" {
            return ParsedMessage::LatchClear(if let Some((member_id, _)) = latch_state {
                member_id
            } else {
                0
            })
        }

        if message.content.starts_with(r"\") {
            return ParsedMessage::UnproxiedMessage
        }

        if message.content.starts_with(r"!") {
            return ParsedMessage::Command(
                MessageParser::parse_command(message, secondary_message, system_config, latch_state)
            );
        }

        if CORRECTION_REGEX.is_match(message.content.as_str()) {
            if let Some(parse) = MessageParser::check_correction(message, secondary_message) {
                return parse
            }
        }

        if let Some(parse) = MessageParser::check_member_patterns(message, secondary_message, system_config) {
            return parse
        }

        if let Some(parse) = MessageParser::check_autoproxy(message, latch_state) {
            return parse
        }

        // If nothing else
        ParsedMessage::UnproxiedMessage
    }

    fn parse_command(message: &FullMessage, secondary_message: Option<&FullMessage>, system_config: &System, latch_state: Option<(MemberId, Timestamp)>) -> Command {
        let mut words = message.content.strip_prefix("!").unwrap().split_whitespace();
        let first_word = words.next();

        match first_word {
            Some(command_name) => match command_name {
                "edit" => {
                    let editing_member = Self::get_member_id_from_user_id(secondary_message.as_ref().unwrap().author.id, system_config).unwrap();
                    return Command::Edit(editing_member, secondary_message.unwrap().id, words.remainder().unwrap().to_string())
                },
                "nick" => {
                    if let Some(member) = MessageParser::match_member(words.next(), system_config) {
                        return Command::Nick(member, words.remainder().unwrap().to_string());
                    }
                },
                "reproxy" => {
                    if let Some(member) = MessageParser::match_member(words.next(), system_config) {
                        return Command::Reproxy(member, secondary_message.unwrap().id);
                    }
                },
                _ => (),
            },
            None => return Command::UnknownCommand,
        }

        // Attempt matching !s
        if message.content.chars().nth(1).unwrap() == 's' {
            let separator = message.content.chars().nth(2).unwrap();
            let parts: Vec<&str> = message.content.split(separator).collect();

            if parts.len() != 3 && parts.len() != 4 {
                return Command::InvalidCommand
            }

            let pattern = parts.get(1).unwrap();
            let replacement = parts.get(2).unwrap();
            let flags = parts.get(3).unwrap_or(&"");

            let mut global = false;
            let mut regex = RegexBuilder::new(pattern);

            for flag in flags.chars() {match flag {
                'i' => {regex.case_insensitive(true);},
                'm' => {regex.multi_line(true);},
                'g' => {global = true;},
                'x' => {regex.ignore_whitespace(true);},
                'R' => {regex.crlf(true);},
                's' => {regex.dot_matches_new_line(true);},
                'U' => {regex.swap_greed(true);},
                _ => {return Command::InvalidCommand;},
            }};

            let regex = regex.build().unwrap();

            let original_content = &secondary_message.as_ref().unwrap().content;
            let new_content = if global {
                regex.replace_all(original_content.as_str(), *replacement)
            } else {
                regex.replace(original_content.as_str(), *replacement)
            };

            let editing_member = Self::get_member_id_from_user_id(secondary_message.as_ref().unwrap().author.id, system_config).unwrap();
            return Command::Edit(editing_member, secondary_message.as_ref().unwrap().id, new_content.to_string());
        }

        // If unable to parse
        Command::UnknownCommand
    }

    fn check_correction(message: &FullMessage, secondary_message: Option<&FullMessage>) -> Option<ParsedMessage> {
        None
    }

    fn check_member_patterns(message: &FullMessage, secondary_message: Option<&FullMessage>, system_config: &System) -> Option<ParsedMessage> {
        let matches_prefix = system_config.members.iter().enumerate().find_map(|(member_id, member)|
            Some((member_id, member.matches_proxy_prefix(&message)?))
        );

        if let Some((member_id, matched_content)) = matches_prefix {
            if matched_content.trim() == "*" {
                Some(ParsedMessage::Command(Command::Reproxy(member_id, secondary_message.unwrap().id)))
            } else if matched_content.trim() != "" {
                Some(ParsedMessage::ProxiedMessage {
                    member_id,
                    message_content: matched_content.to_string(),
                    latch: true,
                })
            } else {
                Some(ParsedMessage::SetProxyAndDelete(member_id))
            }
        } else {
            None
        }
    }

    fn check_autoproxy(message: &FullMessage, latch_state: Option<(MemberId, Timestamp)>) -> Option<ParsedMessage> {
        if let Some((member_id, _)) = latch_state {
            Some(ParsedMessage::ProxiedMessage {
                member_id,
                message_content: message.content.clone(),
                latch: true,
            })
        } else {
            None
        }
    }

    fn match_member(maybe_mention: Option<&str>, system_config: &System) -> Option<MemberId> {
        if let Some(maybe_mention) = maybe_mention {
            if let Ok(mention) = Id::<UserMarker>::parse(maybe_mention) {
                return MessageParser::get_member_id_from_user_id(mention, system_config)
            }
        }

        None
    }

    pub fn get_member_id_from_user_id(user_id: UserId, system_config: &System) -> Option<MemberId> {
        system_config.members.iter().enumerate()
            .filter(|(_id, m)| m.user_id.is_some())
            .find(|(_id, m)| m.user_id.unwrap() == user_id)
            .map(|(id, _m)| id)
    }
}

impl crate::config::Member {
    pub fn matches_proxy_prefix<'a>(&self, message: &'a FullMessage) -> Option<&'a str> {
        match self.message_pattern.captures(message.content.as_str()) {
            None => None,
            Some(captures) => match captures.name("content") {
                None => None,
                Some(matched_content) => Some(matched_content.as_str()),
            },
        }
    }
}

