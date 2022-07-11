use std::{
    error::Error,
    io,
    marker::PhantomData,
    os::unix::{
        net::UnixStream,
        prelude::{AsRawFd, RawFd},
    },
    pin::Pin,
    sync::{mpsc, Arc},
    task::{Context, Poll},
};

use bytes::BytesMut;
use futures::{ready, Sink, Stream, TryStream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_serde::Deserializer;
use tokio_serde::{formats::MessagePack, Serializer};
use tokio_serde::{Framed as SerdeFramed, *};
use tokio_util::codec::Framed;
use tokio_util::codec::LengthDelimitedCodec;

use self::{channel::AsyncChannel, handles::HandlesMove};

pub mod channel;
pub mod handles;

#[pin_project]
#[derive(Debug)]
pub struct FramedWithHandles<Transport, Item, SinkItem, Codec> {
    #[pin]
    inner: Transport,
    #[pin]
    codec: Codec,
    item: PhantomData<(Item, SinkItem)>,
}

#[pin_project]
pub struct TransportWithHandles<S, Item, SinkItem, Codec> {
    #[pin]
    inner: SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>,
}

impl<S, Item, SinkItem, Codec> TransportWithHandles<S, Item, SinkItem, Codec> {
    /// Returns the inner transport over which messages are sent and received.
    pub fn get_ref(&self) -> &S {
        self.inner.get_ref().get_ref()
    }
}

mod fd_wrapper {
    use std::{marker::PhantomData, pin::Pin, sync::Arc};

    use bytes::{Bytes, BytesMut};
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio_serde::{Deserializer, Serializer};

    use super::channel::ChannelMetadata;

    #[derive(Clone)]
    #[pin_project]
    pub struct ChannelMetadataCodec<Codec, Item, SinkItem> {
        metadata: Arc<ChannelMetadata>,
        #[pin]
        codec: Codec,
        phantom: PhantomData<(Item, SinkItem)>,
    }

    impl<Codec, Item, SinkItem> ChannelMetadataCodec<Codec, Item, SinkItem>
    where
        Codec: Deserializer<Item> + Serializer<SinkItem>,
    {
        pub fn new(codec: Codec, metadata: Arc<ChannelMetadata>) -> Self {
            Self {
                codec,
                metadata,
                phantom: PhantomData,
            }
        }
    }

    impl<Codec, Item, SinkItem> Deserializer<Item> for ChannelMetadataCodec<Codec, Item, SinkItem>
    where
        for<'a> Item: Deserialize<'a>,
        Codec: Deserializer<Item>,
    {
        type Error = Codec::Error;

        fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
            self.project().codec.deserialize(src)
        }
    }

    impl<Codec, Item, SinkItem> Serializer<SinkItem> for ChannelMetadataCodec<Codec, Item, SinkItem>
    where
        SinkItem: Serialize,
        Codec: Serializer<SinkItem>,
    {
        type Error = Codec::Error;

        fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
            self.project().codec.serialize(item)
        }
    }
}

impl<Codec, Item, SinkItem>
    TransportWithHandles<
        AsyncChannel,
        Item,
        SinkItem,
        fd_wrapper::ChannelMetadataCodec<Codec, Item, SinkItem>,
    >
where
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
    Codec: Serializer<SinkItem> + Deserializer<Item>,
{
    pub fn new(channel: channel::AsyncChannel, codec: Codec) -> Self {
        let codec = fd_wrapper::ChannelMetadataCodec::new(codec, channel.share_metadata());
        TransportWithHandles {
            inner: SerdeFramed::new(Framed::new(channel, LengthDelimitedCodec::new()), codec),
        }
    }
}

impl<S, Item, SinkItem, Codec, CodecError> Stream for TransportWithHandles<S, Item, SinkItem, Codec>
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
            .map_ok(|i| i.into())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

impl<S, Item, SinkItem, Codec, CodecError> Sink<SinkItem>
    for TransportWithHandles<S, Item, SinkItem, Codec>
where
    S: AsyncWrite + AsyncRead,
    Item: for<'a> Deserialize<'a>,
    Codec: Serializer<SinkItem>,
    CodecError: Into<Box<dyn std::error::Error + Send + Sync>>,
    SerdeFramed<Framed<S, LengthDelimitedCodec>, Item, SinkItem, Codec>:
        Sink<SinkItem, Error = CodecError>,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .inner
            .poll_ready(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> Result<(), Self::Error> {
        self.project()
            .inner
            .start_send(item)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .inner
            .poll_flush(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .inner
            .poll_close(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

#[cfg(test)]
mod tests {
    use super::channel;
    use crate::{
        assert_child_exit, fork,
        sockets::transport::{channel::AsyncChannel, TransportWithHandles},
    };
    use futures::{sink::SinkExt, StreamExt};
    use serde::{Deserialize, Serialize};
    use tokio_serde::formats::MessagePack;
    #[derive(Serialize, Deserialize, Debug)]
    struct ExampleData {
        #[serde(skip_serializing, skip_deserializing)]
        channel: Option<super::channel::Channel>,
        string: String,
    }

    impl super::handles::HandlesMove for ExampleData {
        fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
        where
            M: super::handles::HandlesTransfer,
        {
            self.channel.move_handles(mover)
        }
    }

    #[test]
    fn test_dupa() {
        let data = ExampleData {
            channel: None,
            string: "test".to_owned(),
        };

        let (local, remote) = channel::Channel::pair().unwrap();

        let pid = fork::safer_fork((remote), |remote| {
            fork::tests::set_default_child_panic_handler();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();

            let _guard = runtime.enter();

            let remote: AsyncChannel = remote.take().try_into().unwrap();
            let data = ExampleData {
                channel: None,
                string: "test".to_owned(),
            };
            let codec: MessagePack<ExampleData, ExampleData> = MessagePack::default();
            let mut transport = TransportWithHandles::new(remote, codec);

            runtime.block_on(transport.send(data)).unwrap();
        })
        .unwrap();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();

        let local: AsyncChannel = local.try_into().unwrap();
        let codec: MessagePack<ExampleData, ExampleData> = MessagePack::default();
        let mut transport = TransportWithHandles::new(local, codec);

        let res = runtime.block_on(transport.next()).unwrap().unwrap();
        println!("{:?}", res);

        assert_child_exit!(pid);
    }
}
