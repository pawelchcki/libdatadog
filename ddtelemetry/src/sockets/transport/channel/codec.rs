use std::{marker::PhantomData, pin::Pin, sync::Arc};

use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use tokio_serde::{formats::MessagePack, Deserializer, Serializer};

use crate::sockets::transport::handles::{HandlesMove, HandlesReceive};

use super::ChannelMetadata;

pub type DefaultInnerCodec<Item, SinkItem> = MessagePack<Item, SinkItem>;

pub type DefaultCodec<Item, SinkItem> = ChannelMetadataCodec<DefaultInnerCodec<Item, SinkItem>, Item, SinkItem>;

#[derive(Clone)]
#[pin_project]
pub struct ChannelMetadataCodec<Codec, Item, SinkItem> {
    metadata: ChannelMetadata,
    #[pin]
    codec: Codec,
    phantom: PhantomData<(Item, SinkItem)>,
}

impl<Codec, Item, SinkItem> ChannelMetadataCodec<Codec, Item, SinkItem>
where
    Codec: Deserializer<Item> + Serializer<SinkItem>,
{
    pub fn new(codec: Codec, metadata: ChannelMetadata) -> Self {
        Self {
            codec,
            metadata,
            phantom: PhantomData,
        }
    }
}

impl<Item, SinkItem> From<ChannelMetadata>
    for ChannelMetadataCodec<DefaultInnerCodec<Item, SinkItem>, Item, SinkItem>
where
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
{
    fn from(metadata: ChannelMetadata) -> Self {
        ChannelMetadataCodec::new(DefaultInnerCodec::default(), metadata)
    }
}

impl<Item, SinkItem> From<&ChannelMetadata>
    for ChannelMetadataCodec<DefaultInnerCodec<Item, SinkItem>, Item, SinkItem>
where
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
{
    fn from(metadata: &ChannelMetadata) -> Self {
        ChannelMetadataCodec::new(DefaultInnerCodec::default(), metadata.clone())
    }
}

impl<Codec, Item, SinkItem> Deserializer<Item> for ChannelMetadataCodec<Codec, Item, SinkItem>
where
    for<'a> Item: Deserialize<'a> + HandlesReceive,
    Codec: Deserializer<Item>,
    <Codec as tokio_serde::Deserializer<Item>>::Error: From<std::io::Error>,
{
    type Error = Codec::Error;

    fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
        let projection = self.project();
        let mut item = projection.codec.deserialize(src)?;
        // item.receive_handles(projection.metadata)
        //     .map_err(|e| e.into())?;
        Ok(item)
    }
}

impl<Codec, Item, SinkItem> Serializer<SinkItem> for ChannelMetadataCodec<Codec, Item, SinkItem>
where
    SinkItem: Serialize + HandlesMove,
    Codec: Serializer<SinkItem>,
{
    type Error = Codec::Error;

    fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
        let projection = self.project();

        item.move_handles(projection.metadata).unwrap();
        projection.codec.serialize(item)
    }
}
