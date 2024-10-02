use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use futures::future::join_all;
use twilight_gateway::{Intents, Shard, ShardId};
use twilight_http::{request::channel::reaction::RequestReactionType, Client};
use twilight_http::error::Error as TwError;
use twilight_model::gateway::{
    payload::outgoing::{update_presence::UpdatePresencePayload, UpdatePresence},
    OpCode,
};
use twilight_model::{
    channel,
    channel::{
        message::{AllowedMentions, MentionType, MessageType},
        Message,
    },
    id::{marker::UserMarker, Id},
};
use twilight_model::http::attachment::Attachment;

use super::{MemberId, MessageEvent, Status, SystemEvent, UserId, MessageId, ChannelId};

#[derive(Clone)]
pub struct Bot {
    member_id: MemberId,
    reference_user_id: UserId,
    last_presence: Arc<Mutex<Status>>,
    message_handler: Option<Sender<MessageEvent>>,
    system_handler: Option<Sender<SystemEvent>>,
    shard: Arc<Mutex<Shard>>,
    client: Arc<Mutex<Client>>,
}

impl Bot {
    pub fn new(
        member_id: MemberId,
        config: &crate::config::Member,
        reference_user_id: UserId,
    ) -> Self {
        let intents = Intents::GUILD_MEMBERS
            | Intents::GUILD_PRESENCES
            | Intents::GUILD_MESSAGES
            | Intents::MESSAGE_CONTENT;

        Self {
            member_id,
            reference_user_id,
            last_presence: Arc::new(Mutex::new(Status::Online)),
            message_handler: None,
            system_handler: None,
            shard: Arc::new(Mutex::new(Shard::new(
                ShardId::ONE,
                config.discord_token.clone(),
                intents,
            ))),
            client: Arc::new(Mutex::new(Client::new(config.discord_token.clone()))),
        }
    }

    pub fn set_message_handler(&mut self, handler: Sender<MessageEvent>) {
        self.message_handler = Some(handler);
    }

    pub fn set_system_handler(&mut self, handler: Sender<SystemEvent>) {
        self.system_handler = Some(handler);
    }

    pub async fn set_status(&self, status: Status) {
        let mut last_status = self.last_presence.lock().await;

        if status == *last_status {
            return
        }

        let mut shard = self.shard.lock().await;

        shard
            .command(&UpdatePresence {
                d: UpdatePresencePayload {
                    activities: Vec::new(),
                    afk: false,
                    since: None,
                    status,
                },
                op: OpCode::PresenceUpdate,
            })
            .await
            .expect("Could not send command to gateway");

        *last_status = status;
    }

    pub async fn delete_message(&self, channel_id: ChannelId, message_id: MessageId) -> Result<(), TwError> {
        let client = self.client.lock().await;
        let delete_result = client.delete_message(channel_id, message_id).await;

        match delete_result {
            Err(err) => {
                match &err.kind() {
                    twilight_http::error::ErrorType::Response { body: _, error, status: _ } => match error {
                        twilight_http::api_error::ApiError::General(err) => {
                            // Code for "Missing Permissions": https://discord.com/developers/docs/topics/opcodes-and-status-codes#json-json-error-codes
                            if err.code == 50013 {
                                println!("ERROR: Client {} doesn't have permissions to delete message", self.member_id);
                                let _ = client.create_reaction(
                                    channel_id,
                                    message_id,
                                    &RequestReactionType::Unicode { name: "🔐" }
                                ).await;
                            }
                        },
                        _ => (),
                    },
                    _ => (),
                };

                Err(err)
            },
            _ => Ok(()),
        }
    }

    pub async fn duplicate_message(&self, message: &Message, content: &str) -> Result<channel::Message, MessageDuplicateError> {
        let client = self.client.lock().await;

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

    pub fn start_listening(&self) {
        self.clone().start_listen_task()
    }

    fn start_listen_task(self) {
        tokio::spawn(async move {
            loop {
                let next_event = { self.shard.lock().await.next_event().await };

                match next_event {
                    Err(source) => {
                        if let Some(channel) = &self.system_handler {
                            channel
                                .send(SystemEvent::GatewayError(self.member_id, source.to_string()))
                                .await;

                            if source.is_fatal() {
                                channel.send(SystemEvent::GatewayClosed(self.member_id)).await;
                                break;
                            }
                        }
                    }
                    Ok(event) => match event {
                        twilight_gateway::Event::Ready(_) => {
                            if let Some(channel) = &self.system_handler {
                                channel
                                    .send(SystemEvent::GatewayConnected(self.member_id))
                                    .await;
                            }
                        }

                        twilight_gateway::Event::MessageCreate(message_create) => {
                            let message = message_create.0;

                            if message.author.id != self.reference_user_id {
                                continue;
                            }

                            if let Some(channel) = &self.message_handler {
                                channel
                                    .send((message.timestamp, message))
                                    .await;
                            }
                        }

                        twilight_gateway::Event::MessageUpdate(message_update) => {
                            if message_update.author.is_none()
                                || message_update.author.as_ref().unwrap().id != self.reference_user_id
                            {
                                continue;
                            }

                            if message_update.edited_timestamp.is_none() {
                                println!("Message update but no edit timestamp");
                                continue;
                            }

                            if message_update.content.is_none() {
                                println!("Message update but no content");
                                continue;
                            }

                            let message = self.client
                                .lock()
                                .await
                                .message(message_update.channel_id, message_update.id)
                                .await
                                .expect("Could not load message")
                                .model()
                                .await
                                .expect("Could not deserialize message");

                            if let Some(channel) = &self.message_handler {
                                channel
                                    .send((message_update.edited_timestamp.unwrap(), message))
                                    .await;
                            }
                        }

                        _ => (),
                    },
                }
            }
        });
    }
}


#[derive(Debug)]
pub enum MessageDuplicateError {
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
