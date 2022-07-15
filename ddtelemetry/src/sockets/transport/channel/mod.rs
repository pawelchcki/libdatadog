mod platform;
pub use platform::*;
mod codec;
pub use codec::*;
use serde::{Deserialize, Serialize};
use tarpc::serde_transport::Transport;

use super::handles::{HandlesMove, HandlesReceive};

pub type SymmetricalTransport<S, T, Codec> = Transport<S, T, T, Codec>;

impl<Item, SinkItem> From<AsyncChannel>
    for Transport<
        AsyncChannel,
        Item,
        SinkItem,
        ChannelMetadataCodec<DefaultCodec<Item, SinkItem>, Item, SinkItem>,
    >
where
    Item: for<'de> Deserialize<'de> + HandlesReceive,
    SinkItem: Serialize + HandlesMove,
{
    fn from(channel: AsyncChannel) -> Self {
        let codec = ChannelMetadataCodec::from(&channel.metadata);
        Transport::from((channel, codec))
    }
}

impl<Item, SinkItem> TryFrom<Channel>
    for Transport<
        AsyncChannel,
        Item,
        SinkItem,
        ChannelMetadataCodec<DefaultCodec<Item, SinkItem>, Item, SinkItem>,
    >
where
    Item: for<'de> Deserialize<'de> + HandlesReceive,
    SinkItem: Serialize + HandlesMove,
{
    type Error = <AsyncChannel as TryFrom<Channel>>::Error;

    fn try_from(channel: Channel) -> Result<Self, Self::Error> {
        Ok(Self::from(AsyncChannel::try_from(channel)?))
    }
}

