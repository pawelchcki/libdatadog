
use std::{
    fs::File,
    io::{self, Seek}, sync::Mutex,
};

use super::{
    channel,
    handles::{BetterHandle, TransferHandles},
};
use crate::{
    assert_child_exit, fork::{self, safer_fork},
    sockets::transport::{
        channel::{AsyncChannel},
        SymmetricalTransport, Transport,
    },
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::io::Write;
use tarpc::{
    context,
    server::{Channel},
};
use tokio::io::{AsyncBufReadExt, BufReader};
use serial_test::serial;

#[derive(Serialize, Deserialize, Debug)]
struct ExampleData {
    channel: BetterHandle<channel::Channel>,
    string: String,
}

impl super::handles::TransferHandles for ExampleData {
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: super::handles::HandlesTransport,
    {
        self.channel.move_handles(mover)
    }

    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: super::handles::HandlesTransport,
    {
        self.channel.receive_handles(provider)
    }
}

#[test]
// #[serial]
fn test_basic_com() {
    let (local, remote) = channel::Channel::pair().unwrap();

    let pid = fork::safer_fork(remote, |remote| {
        fork::tests::set_default_child_panic_handler();
        let runtime = start_runtime();
        let _g = runtime.enter();

        let (ch, _) = channel::Channel::pair().unwrap();

        let data = ExampleData {
            channel: BetterHandle::from(ch),
            string: "test".to_owned(),
        };

        let mut transport = SymmetricalTransport::try_from(remote).unwrap();

        runtime.block_on(transport.send(data)).unwrap();
    })
    .unwrap();

    let runtime = start_runtime();
    let _g = runtime.enter();

    let local: AsyncChannel = local.try_into().unwrap();
    let mut transport = SymmetricalTransport::from(local);

    let res: ExampleData = runtime.block_on(transport.next()).unwrap().unwrap();
    assert_eq!(res.string, "test");
    assert_child_exit!(pid);
}

#[tarpc::service]
trait World {
    /// Returns a greeting for name.
    async fn read_from_handle(h: BetterHandle<File>) -> String;
}

#[derive(Clone)]
struct HelloServer;
use tracing_subscriber::fmt::format::FmtSpan;

impl TransferHandles for WorldResponse {
    fn move_handles<Transport: super::handles::HandlesTransport>(
        &self,
        _: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }

    fn receive_handles<Transport: super::handles::HandlesTransport>(
        &mut self,
        _: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }
}

impl TransferHandles for WorldRequest {
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: super::handles::HandlesTransport,
    {
        match self {
            WorldRequest::ReadFromHandle  { h } => mover.move_handle(h.clone()),
            _ => Ok(()),
        }
    }


    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: super::handles::HandlesTransport,
    {
        match self {
            WorldRequest::ReadFromHandle { h } => h.receive_handles(provider),
            _ => Ok(()),
        }
    }
}

#[tarpc::server]
impl World for HelloServer {
    async fn read_from_handle(self, _: context::Context, h: BetterHandle<File>) -> String {
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
// #[serial]
fn test_inprocess_rpc() {
    let runtime = start_runtime();
    let _r = runtime.enter();

    let (local, remote) = channel::Channel::pair().unwrap();

    let server = tarpc::server::BaseChannel::with_defaults(Transport::try_from(local).unwrap());
    runtime.spawn(server.execute(HelloServer.serve()));

    let client = WorldClient::new(
        tarpc::client::Config::default(),
        Transport::try_from(remote).unwrap(),
    )
    .spawn();

    let mut file = tempfile::tempfile().unwrap();
    writeln!(file, "Message sent using a FD").unwrap();
    file.rewind().unwrap();

    let echoed = runtime
        .block_on(client.read_from_handle(context::current(), file.into()))
        .unwrap();

    assert_eq!("Message sent using a FD", echoed);
}

#[test]
// #[serial]
fn test_interprocess_rpc() {
    let (local, remote) = channel::Channel::pair().unwrap();
    let child = safer_fork(remote, |remote| {
        fork::tests::set_default_child_panic_handler();
        let runtime = start_runtime();
        let _r = runtime.enter();

        let server = tarpc::server::BaseChannel::with_defaults(Transport::try_from(remote).unwrap());
        runtime.block_on(server.execute(HelloServer.serve()));
    }).unwrap();
    
    let runtime = start_runtime();
    let _r = runtime.enter();

    let client = WorldClient::new(
        tarpc::client::Config::default(),
        Transport::try_from(local).unwrap(),
    )
    .spawn();

    let mut file = tempfile::tempfile().unwrap();
    writeln!(file, "Message sent using a FD").unwrap();
    file.rewind().unwrap();

    let echoed = runtime
        .block_on(client.read_from_handle(context::current(), file.into()))
        .unwrap();

    assert_eq!("Message sent using a FD", echoed);
    drop(client);
    assert_child_exit!(child);

}

fn start_runtime() -> tokio::runtime::Runtime {
    // setup debug trace output
    let collector = tracing_subscriber::fmt()
        .with_writer(io::stdout)
        .with_span_events(FmtSpan::FULL)
        .with_max_level(tracing::Level::TRACE)
        .finish();

    // tracing::subscriber::set_global_default(collector).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    
    runtime
}
