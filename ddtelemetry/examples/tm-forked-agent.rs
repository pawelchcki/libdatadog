use std::{
    io,
    time::{Duration, SystemTime}, sync::{atomic::AtomicU64, Arc},
};

use ddtelemetry::{
    fork::safer_fork,
    sockets::transport::{
        channel::{self},
        handles::{HandlesTransport, TransferHandles},
        BlockingChannel, Transport,
    },
};
use tarpc::{
    context,
    server::{self, Channel},
};
use tokio_serde::formats::Bincode;
use tracing_subscriber::fmt::format::FmtSpan;

#[tarpc::service]
trait World {
    /// Returns a greeting for name.
    async fn hello(name: String) -> ();
}

#[derive(Clone)]
struct HelloServer {
    cnt: Arc<AtomicU64>
}
use futures::future::{self, Ready};

impl TransferHandles for WorldResponse {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }
}

impl TransferHandles for WorldRequest {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }
}

impl World for HelloServer {
    // Each defined rpc generates two items in the trait, a fn that serves the RPC, and
    // an associated type representing the future output by the fn.

    type HelloFut = Ready<()>;

    fn hello(self, ctx: tarpc::context::Context, name: String) -> Self::HelloFut {
        let cnt = self.cnt.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        println!("req: {}", cnt);
        future::ready(())
    }
}

impl Default for HelloServer {
    fn default() -> Self {
        Self { cnt: Default::default() }
    }
}

fn setup_tracing() {
    let collector = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_span_events(FmtSpan::FULL)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(collector).unwrap();
}

fn main() -> anyhow::Result<()> {
    // setup_tracing();

    // let _guard = runtime.enter();
    let pair = channel::Channel::pair().unwrap();
    let _child = safer_fork(&pair, |pair| 
    {
        let remote = pair.remote().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = runtime.enter();
        let server =
            tarpc::server::BaseChannel::with_defaults(Transport::try_from(remote).unwrap());

        runtime.block_on(server.execute(HelloServer::default().serve()));
    })
    .unwrap();

    let mut local = BlockingChannel::from(pair.local().unwrap());

    for n in 0..100000 {
        local.send_and_forget(WorldRequest::Hello { name: "ping".to_owned() }).unwrap();
        println!("sent: {}", n);
    }
    
    std::thread::sleep(Duration::from_secs(2));
    drop(local);
    println!("dropping {}, self: {}", _child, unsafe {libc::getpid()});
    std::thread::sleep(Duration::from_secs(120));

    Ok(())
}
