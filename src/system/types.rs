use twilight_model::channel::Message;
use twilight_model::id::marker::{ChannelMarker, MessageMarker, UserMarker};
use twilight_model::id::Id;
use twilight_model::util::Timestamp;

pub type MemberId = usize;
pub type MessageId = Id<MessageMarker>;
pub type ChannelId = Id<ChannelMarker>;
pub type UserId = Id<UserMarker>;

pub type Status = twilight_model::gateway::presence::Status;

pub type MessageEvent = (Timestamp, Message);
pub type ReactionEvent = (Timestamp, ());
pub type CommandEvent = (Timestamp, ());

pub enum SystemEvent {
    // Process of operation
    GatewayConnected(MemberId),
    GatewayError(MemberId, String),
    GatewayClosed(MemberId),
    AllGatewaysConnected,

    // User event handling
    NewMessage(MessageEvent),
    EditedMessage(MessageEvent),
    NewReaction(ReactionEvent),

    // Command handling
    NewCommand(CommandEvent),

    // Autoproxy
    AutoproxyTimeout(Timestamp),
}
