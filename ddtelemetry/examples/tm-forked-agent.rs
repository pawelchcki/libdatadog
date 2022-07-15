use std::{io, time::{SystemTime, Duration}};

use ddtelemetry::sockets::transport::{channel::{self, AsyncChannel}, handles::{HandlesMove, HandlesTransfer, HandlesReceive}, TransportWithHandles, fd_wrapper::ChannelMetadataCodec};
use tarpc::{server::{self, Channel}, context};
use tokio_serde::formats::Bincode;
use tracing_subscriber::fmt::format::FmtSpan;

#[tarpc::service]
trait World {
    /// Returns a greeting for name.
    async fn hello(name: String) -> String;
}

#[derive(Clone)]
struct HelloServer;
use futures::{
    future::{self, Ready},
};

impl HandlesMove for WorldResponse {
}

impl HandlesMove for WorldRequest {
}

impl HandlesReceive for WorldRequest {
}

impl HandlesReceive for WorldResponse {}

impl World for HelloServer {
    // Each defined rpc generates two items in the trait, a fn that serves the RPC, and
    // an associated type representing the future output by the fn.

    type HelloFut = Ready<String>;

    fn hello(self, _: tarpc::context::Context, name: String) -> Self::HelloFut {
        future::ready(name)
    }
}


    fn setup_runtime()  {
        let collector = tracing_subscriber::fmt()
            .with_writer(io::stderr)
            .with_span_events(FmtSpan::FULL)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(collector).unwrap();
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
            ChannelMetadataCodec<
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_runtime();

    // let _guard = runtime.enter();
    let (local, remote) = channel::Channel::pair().unwrap();

    let remote = remote.take();
    let server = build_server(local);
    tokio::spawn(server.execute(HelloServer.serve()));

    let client = build_client(remote);
    let mut ctx = context::current();
    ctx.deadline = SystemTime::now() + Duration::from_secs(1000);

    let hello = client.hello(ctx, "Stim".to_string()).await?;

    println!("Echo: {:?}", hello);
    Ok(())
}
