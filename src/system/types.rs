pub use twilight_model::channel::Message as TwiMessage;
use twilight_model::gateway::payload::incoming::MessageUpdate as PartialMessage;
use twilight_model::id::marker::{ChannelMarker, MessageMarker, UserMarker};
use twilight_model::id::Id;
use twilight_model::util::Timestamp;

pub type MemberId = usize;
pub type MessageId = Id<MessageMarker>;
pub type ChannelId = Id<ChannelMarker>;
pub type UserId = Id<UserMarker>;
pub type FullMessage = TwiMessage;

pub type Status = twilight_model::gateway::presence::Status;

#[derive(Clone)]
pub enum Message {
    Complete(FullMessage),
    Partial(PartialMessage, MemberId),
}

pub type MessageEvent = (Timestamp, Message);
pub type ReactionEvent = (Timestamp, ());
pub type CommandEvent = (Timestamp, ());

pub enum SystemEvent {
    // Process of operation
    GatewayConnected(MemberId),
    GatewayError(MemberId, String),
    GatewayClosed(MemberId),
    AllGatewaysConnected,
    RefetchMessage(MemberId, MessageId, ChannelId),

    // User event handling
    NewMessage(Timestamp, FullMessage),
    EditedMessage(MessageEvent),
    NewReaction(ReactionEvent),

    // Command handling
    NewCommand(CommandEvent),

    // Autoproxy
    AutoproxyTimeout(Timestamp),
}
