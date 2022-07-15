use std::{
    io,
    pin::Pin,
    task::{Context, Poll}, error::Error,
};

use futures::{ Sink, Stream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_serde::Deserializer;
use tokio_serde::{ Serializer};
use tokio_serde::{Framed as SerdeFramed};
use tokio_util::codec::Framed;
use tokio_util::codec::LengthDelimitedCodec;

use self::{channel::{AsyncChannel, ChannelMetadataCodec, DefaultCodec, Channel}, handles::{HandlesReceive, HandlesMove}};

pub mod channel;
pub mod handles;


/// A transport that serializes to, and deserializes from, a byte stream.
#[pin_project]
pub struct Transport<S, Item, SinkItem, Codec> {
    #[pin]
    inner: SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>,
}

impl<S, Item, SinkItem, Codec> Transport<S, Item, SinkItem, Codec> {
    /// Returns the inner transport over which messages are sent and received.
    pub fn get_ref(&self) -> &S {
        self.inner.get_ref().get_ref()
    }
}

impl<S, Item, SinkItem, Codec, CodecError> Stream for Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'a> Deserialize<'a>,
    Codec: Deserializer<Item>,
    CodecError: Into<Box<dyn std::error::Error + Send + Sync>>,
    SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>:
        Stream<Item = Result<Item, CodecError>>,
{
    type Item = io::Result<Item>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<io::Result<Item>>> {
        self.project()
            .inner
            .poll_next(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

impl<S, Item, SinkItem, Codec, CodecError> Sink<SinkItem> for Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem>,
    CodecError: Into<Box<dyn Error + Send + Sync>>,
    SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>:
        Sink<SinkItem, Error = CodecError>,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_ready(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> io::Result<()> {
        self.project()
            .inner
            .start_send(item)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_flush(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_close(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

/// Constructs a new transport from a framed transport and a serialization codec.
pub fn new<S, Item, SinkItem, Codec>(
    framed_io: Framed<S, LengthDelimitedCodec>,
    codec: Codec,
) -> Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem> + Deserializer<Item>,
{
    Transport {
        inner: SerdeFramed::new(framed_io, codec),
    }
}

impl<S, Item, SinkItem, Codec> From<(S, Codec)> for Transport<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem> + Deserializer<Item>,
{
    fn from((io, codec): (S, Codec)) -> Self {
        new(Framed::new(io, LengthDelimitedCodec::new()), codec)
    }
}

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

#[cfg(test)]
mod tests;