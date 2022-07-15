use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ Sink, Stream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_serde::Deserializer;
use tokio_serde::{ Serializer};
use tokio_serde::{Framed as SerdeFramed};
use tokio_util::codec::Framed;
use tokio_util::codec::LengthDelimitedCodec;

use self::{channel::AsyncChannel};

pub mod channel;
pub mod handles;

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

pub mod fd_wrapper {
    use std::{marker::PhantomData, pin::Pin, sync::Arc};

    use bytes::{Bytes, BytesMut};
    use serde::{Deserialize, Serialize};
    
    use tokio_serde::{Deserializer, Serializer};

    use super::{
        channel::ChannelMetadata,
        handles::{HandlesMove, HandlesReceive},
    };

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
        for<'a> Item: Deserialize<'a> + HandlesReceive,
        Codec: Deserializer<Item>,
        <Codec as tokio_serde::Deserializer<Item>>::Error: From<std::io::Error>,
    {
        type Error = Codec::Error;

        fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
            let projection = self.project();
            let mut item = projection.codec.deserialize(src)?;
            item.receive_handles(projection.metadata)
                .map_err(|e| e.into())?;
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
    use std::{
        fs::File,
        io::{self, Seek},
    };

    use super::{
        channel,
        handles::{BetterHandle, HandlesMove, HandlesReceive},
    };
    use crate::{
        assert_child_exit, fork,
        sockets::transport::{channel::AsyncChannel, TransportWithHandles},
    };
    use futures::{sink::SinkExt, StreamExt};
    use serde::{Deserialize, Serialize};
    use std::io::Write;
    use tarpc::{
        context,
        server::{self, Channel},
    };
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio_serde::formats::{Bincode, MessagePack};
    #[derive(Serialize, Deserialize, Debug)]
    struct ExampleData {
        // #[serde(skip_serializing, skip_deserializing)]
        channel: BetterHandle<channel::Channel>,
        string: String,
    }

    impl super::handles::HandlesMove for ExampleData {
        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: super::handles::HandlesTransfer,
        {
            self.channel.move_handles(mover)
        }
    }

    impl super::handles::HandlesReceive for ExampleData {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: super::handles::HandlesProvider,
        {
            self.channel.receive_handles(provider)
        }
    }

    #[test]
    fn test_basic_com() {
        let (local, remote) = channel::Channel::pair().unwrap();

        let pid = fork::safer_fork(remote, |remote| {
            fork::tests::set_default_child_panic_handler();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();

            let _guard = runtime.enter();

            let remote: AsyncChannel = remote.take().try_into().unwrap();
            let (some, _g) = channel::Channel::pair().unwrap();

            let data = ExampleData {
                channel: BetterHandle::from(some),
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
        println!("Received: {:?}", res);

        assert_child_exit!(pid);
    }

    #[tarpc::service]
    trait World {
        /// Returns a greeting for name.
        async fn hello(name: String) -> String;
        async fn send_handle(h: BetterHandle<File>) -> String;
    }

    #[derive(Clone)]
    struct HelloServer;
    use tracing_subscriber::fmt::format::FmtSpan;

    impl HandlesMove for WorldResponse {}

    impl HandlesMove for WorldRequest {
        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: super::handles::HandlesTransfer,
        {
            match self {
                WorldRequest::Hello { name: _ } => Ok(()),
                WorldRequest::SendHandle { h } => mover.move_handle(h.clone()),
            }
        }
    }

    impl HandlesReceive for WorldRequest {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: super::handles::HandlesProvider,
        {
            match self {
                WorldRequest::SendHandle { h } => h.receive_handles(provider),
                _ => Ok(()),
            }
        }
    }

    impl HandlesReceive for WorldResponse {}

    #[tarpc::server]
    impl World for HelloServer {
        // Each defined rpc generates two items in the trait, a fn that serves the RPC, and
        // an associated type representing the future output by the fn.
        async fn hello(self, _: context::Context, name: String) -> String {
            name
        }
        async fn send_handle(self, _: context::Context, h: BetterHandle<File>) -> String {
            let f: File = h.try_into().unwrap();
            let f = tokio::fs::File::from_std(f);

            // let l: Vec<String> =
            let r = BufReader::new(f)
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap();
            r
        }
    }

    #[test]
    fn test_bla() {
        let runtime = setup_runtime();

        let _guard = runtime.enter();
        let (local, remote) = channel::Channel::pair().unwrap();

        let remote = remote.take();
        let server = build_server(local);
        runtime.spawn(server.execute(HelloServer.serve()));

        let client = build_client(remote);
        let mut file = tempfile::tempfile().unwrap();
        writeln!(file, "Yellow").unwrap();
        file.rewind().unwrap();

        let hello = runtime
            .block_on(client.send_handle(context::current(), file.into()))
            .unwrap();

        println!("Echo: {}", hello);
    }

    fn setup_runtime() -> tokio::runtime::Runtime {
        let collector = tracing_subscriber::fmt()
            .with_writer(io::stderr)
            .with_span_events(FmtSpan::FULL)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(collector).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
    }

    fn build_client(remote: channel::Channel) -> WorldClient {
        let client_codec = Bincode::default();
        let client_transport = TransportWithHandles::new(remote.try_into().unwrap(), client_codec);
        let client = WorldClient::new(tarpc::client::Config::default(), client_transport).spawn();
        client
    }

    fn build_server(
        local: channel::Channel,
    ) -> server::BaseChannel<
        WorldRequest,
        WorldResponse,
        TransportWithHandles<
            AsyncChannel,
            tarpc::ClientMessage<WorldRequest>,
            tarpc::Response<WorldResponse>,
            super::fd_wrapper::ChannelMetadataCodec<
                Bincode<tarpc::ClientMessage<WorldRequest>, tarpc::Response<WorldResponse>>,
                tarpc::ClientMessage<WorldRequest>,
                tarpc::Response<WorldResponse>,
            >,
        >,
    > {
        let codec = Bincode::default();
        let server_transport = TransportWithHandles::new(local.try_into().unwrap(), codec);
        let server = tarpc::server::BaseChannel::with_defaults(server_transport);
        server
    }
}
