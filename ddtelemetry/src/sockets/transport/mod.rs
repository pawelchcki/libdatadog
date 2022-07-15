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
        sockets::transport::{channel::{AsyncChannel, ChannelMetadataCodec, SymmetricalTransport}, self},
    };
    use futures::{SinkExt, StreamExt};
    use serde::{Deserialize, Serialize};
    use std::io::Write;
    use tarpc::{
        context,
        server::{self, Channel}, serde_transport::Transport,
    };
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio_serde::formats::{Bincode, MessagePack};
    #[derive(Serialize, Deserialize, Debug)]
    struct ExampleData {
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

            let mut transport = SymmetricalTransport::from(remote);

            runtime.block_on(transport.send(data)).unwrap();
        })
        .unwrap();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();

        let local: AsyncChannel = local.try_into().unwrap();
        let mut transport = SymmetricalTransport::from(local);

        let res: ExampleData = runtime.block_on(transport.next()).unwrap().unwrap();
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
        let server = tarpc::server::BaseChannel::with_defaults(Transport::try_from(local).unwrap());
        runtime.spawn(server.execute(HelloServer.serve()));

        let client = WorldClient::new(tarpc::client::Config::default(), Transport::try_from(remote).unwrap()).spawn();
        
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
}
