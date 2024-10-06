mod client;
mod gateway;

use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use twilight_http::error::Error as TwiError;
use twilight_http::request::channel::reaction::RequestReactionType;

pub use super::types::*;
pub use client::MessageDuplicateError;
use gateway::Gateway;
use client::Client;

#[derive(Clone)]
pub struct BotConfig {
    pub member_id: MemberId,
    pub reference_user_id: UserId,
    pub discord_token: String,
    pub last_status: Status,
    pub message_handler: Option<Sender<MessageEvent>>,
    pub system_handler: Option<Sender<SystemEvent>>,
}

pub struct Bot {
    bot_conf: Arc<RwLock<BotConfig>>,
    gateway: Gateway,
    client: Client,
}

impl Bot {
    pub fn new(
        member_id: MemberId,
        config: &crate::config::Member,
        reference_user_id: UserId,
    ) -> Self {
        let bot_conf = Arc::new(RwLock::new(BotConfig {
            member_id,
            reference_user_id,
            discord_token: config.discord_token.clone(),
            last_status: Status::Online,
            message_handler: None,
            system_handler: None,
        }));

        Self {
            gateway: Gateway::new(&config.discord_token, &bot_conf),
            client: Client::new(&config.discord_token, &bot_conf),
            bot_conf,
        }
    }

    pub async fn set_message_handler(&mut self, handler: Sender<MessageEvent>) {
        self.bot_conf.write().await.message_handler = Some(handler);
    }

    pub async fn set_system_handler(&mut self, handler: Sender<SystemEvent>) {
        self.bot_conf.write().await.system_handler = Some(handler);
    }

    pub async fn set_status(&self, status: Status) {
        self.gateway.set_status(status).await;
    }

    pub fn start(&self) {
        self.gateway.start_listening()
    }

    pub async fn fetch_message(&self, message_id: MessageId, channel_id: ChannelId) -> TwiMessage {
        self.client.fetch_message(message_id, channel_id).await
    }

    pub async fn resend_message(&self, message_id: MessageId, channel_id: ChannelId) {
        self.client.resend_message(message_id, channel_id).await;
    }

    pub async fn delete_message(&self, channel_id: ChannelId, message_id: MessageId) -> Result<(), TwiError> {
        self.client.delete_message(channel_id, message_id).await
    }

    pub async fn react_message(&self, channel_id: ChannelId, message_id: MessageId, react: &'_ RequestReactionType<'_>) -> Result<(), TwiError> {
        self.client.react_message(channel_id, message_id, react).await
    }

    pub async fn duplicate_message(&self, message_id: &TwiMessage, content: &str) ->  Result<TwiMessage, MessageDuplicateError> {
        self.client.duplicate_message(message_id, content).await
    }
}

