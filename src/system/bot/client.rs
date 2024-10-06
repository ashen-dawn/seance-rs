use std::sync::Arc;
use futures::future::join_all;
use tokio::sync::RwLock;
use tokio::sync::Mutex;
use twilight_http::client::Client as TwiClient;
use twilight_http::error::Error as TwiError;
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::channel::message::{AllowedMentions, MentionType, MessageType};
use twilight_model::http::attachment::Attachment;

use super::*;

pub struct Client {
    client: Arc<Mutex<TwiClient>>,
    bot_conf: Arc<RwLock<BotConfig>>,
}

impl Client {
    pub fn new(discord_token: &String, bot_conf: &Arc<RwLock<BotConfig>>) -> Self {
        Self {
            client: Arc::new(Mutex::new(TwiClient::new(discord_token.clone()))),
            bot_conf: bot_conf.clone(),
        }
    }

    pub async fn fetch_message(&self, message_id: MessageId, channel_id: ChannelId) -> FullMessage {
        let client = self.client.lock().await;

        client
            .message(channel_id, message_id)
            .await
            .expect("Could not load message")
            .model()
            .await
            .expect("Could not deserialize message")
    }

    pub async fn resend_message(&self, message_id: MessageId, channel_id: ChannelId) {
        let bot_conf = self.bot_conf.read().await;
        let message = self.fetch_message(message_id, channel_id).await;
        let message_channel = bot_conf.message_handler.as_ref().expect("No message handler");

        let timestamp = if message.edited_timestamp.is_some() {
            message.edited_timestamp.unwrap()
        } else {
            message.timestamp
        };

        message_channel
            .send((timestamp, Message::Complete(message, bot_conf.member_id)))
            .await;
    }

    pub async fn delete_message(&self, channel_id: ChannelId, message_id: MessageId) -> Result<(), TwiError> {
        let client = self.client.lock().await;
        let delete_result = client.delete_message(channel_id, message_id).await;
        let member_id = self.bot_conf.read().await.member_id;

        drop(client);

        match delete_result {
            Err(err) => {
                match &err.kind() {
                    twilight_http::error::ErrorType::Response { body: _, error, status: _ } => match error {
                        twilight_http::api_error::ApiError::General(err) => {
                            // Code for "Missing Permissions": https://discord.com/developers/docs/topics/opcodes-and-status-codes#json-json-error-codes
                            if err.code == 50013 {
                                println!("ERROR: Client {} doesn't have permissions to delete message", member_id);
                                let _ = self.react_message(channel_id, message_id, &RequestReactionType::Unicode { name: "🔐" }).await;
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

    pub async fn react_message(&self, channel_id: ChannelId, message_id: MessageId, react: &'_ RequestReactionType<'_>) -> Result<(), TwiError> {
        let _ = self.client.lock().await.create_reaction(
            channel_id,
            message_id,
            react
        ).await;

        return Ok(())
    }

    pub async fn duplicate_message(&self, message: &TwiMessage, content: &str) -> Result<TwiMessage, MessageDuplicateError> {
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
