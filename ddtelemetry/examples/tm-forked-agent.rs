use std::{
    io,
    sync::{atomic::AtomicU64, Arc},
    thread,
    time::Duration,
};

use ddtelemetry::{
    fork::safer_fork,
    sockets::transport::{
        channel::{self},
        handles::{HandlesTransport, TransferHandles},
        BlockingTransport, Transport,
    },
};
use tarpc::{server::Channel, Response};

use tracing_subscriber::fmt::format::FmtSpan;

#[tarpc::service]
trait World {
    /// Returns a greeting for name.
    async fn hello(name: String) -> ();
}

#[derive(Clone, Default)]
struct HelloServer {
    cnt: Arc<AtomicU64>,
}
use futures::future::{self, Ready};

impl TransferHandles for WorldResponse {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        _transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        _transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }
}

impl TransferHandles for WorldRequest {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        _transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        _transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }
}

impl World for HelloServer {
    // Each defined rpc generates two items in the trait, a fn that serves the RPC, and
    // an associated type representing the future output by the fn.

    type HelloFut = Ready<()>;

    fn hello(self, _ctx: tarpc::context::Context, name: String) -> Self::HelloFut {
        let cnt = self.cnt.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        println!("req: {} - len: {}", cnt, name.len());
        future::ready(())
    }
}

#[allow(dead_code)]
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
    let _child = safer_fork(&pair, |pair| {
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

    let mut ch = pair.local().unwrap();
    ch.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    ch.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    
    let ch = BlockingTransport::from(ch);

    let mut joins = vec![];
    for tn in 0..1 {
        let mut ch = ch.clone();
        let th = thread::spawn(move || {
            for _n in (10000 * tn)..(10000 * (tn + 1)) {
                let item: Response<WorldResponse> = ch.send(WorldRequest::Hello {
                    name: (0..1000).map(|_| "ping".to_owned()).collect(),
                })
                .unwrap();
                println!("Received: {:?}", item);
            }
            std::thread::sleep(Duration::from_secs(2));
            println!("Finished sending");
            drop(ch);
        });
        joins.push(th);
    }

    for th in joins {
        th.join().unwrap();
    }
    drop(ch);

    println!("dropping {}, self: {}", _child, unsafe { libc::getpid() });
    std::thread::sleep(Duration::from_secs(120));

    Ok(())
}
