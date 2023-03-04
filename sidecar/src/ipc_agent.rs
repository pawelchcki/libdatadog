use std::{
    fs::File,
    os::{fd::{IntoRawFd, AsRawFd}, unix::net::UnixListener as StdUnixListener},
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};

use datadog_trace_protobuf::pb::TracerPayload;
use ddtelemetry::{
    ipc::setup::{DefaultLiason, Liaison},
    tarpc::tokio_util::sync::CancellationToken,
};
use spawn_worker::{entrypoint, Stdio};
use tokio::{net::UnixListener, select};

use crate::{ipc::{SidecarServer, SidecarTransport}, rust_tracing, tracing::{uploader::Uploader, trace_events::{EventCollector, self}}};

async fn main_loop(listener: UnixListener) -> tokio::io::Result<()> {
    let counter = Arc::new(AtomicI32::new(0));
    let token = CancellationToken::new();
    let cloned_counter = Arc::clone(&counter);
    let cloned_token = token.clone();
    
    let (payload_tx, payload_rx) = tokio::sync::mpsc::channel::<Box<TracerPayload>>(10);
    let uploader = Uploader::init(&crate::config::Config::init());
    tokio::spawn(uploader.into_event_loop(payload_rx));
    
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<trace_events::Event>(100);
    let event_collector = EventCollector::init(&crate::config::Config::init());
    tokio::spawn(event_collector.into_event_loop(payload_tx, event_rx));

    tokio::spawn(async move {
        let mut consecutive_no_active_connections = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            if cloned_counter.load(Ordering::Acquire) <= 0 {
                consecutive_no_active_connections += 1;
            } else {
                consecutive_no_active_connections = 0;
            }

            if consecutive_no_active_connections > 1 {
                cloned_token.cancel();
            }
        }
    });

    let server = SidecarServer::new(event_tx);

    loop {
        let (socket, _) = select! {
            res = listener.accept() => {
                res?
            },
            _ = token.cancelled() => {
                break
            },
        };

        counter.fetch_add(1, Ordering::AcqRel);

        let cloned_counter = Arc::clone(&counter);
        let server = server.clone();
        tokio::spawn(async move {
            server.handle(socket).await;
            cloned_counter.fetch_add(-1, Ordering::AcqRel);
        });
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn ipc_agent_entrypoint() {
    crate::libc_main::wrap_result(|| {
        // rust_tracing::enable_tracing().unwrap();
        eprintln!("starting sidecar");
        let listener: StdUnixListener = spawn_worker::recv_passed_fd()?.try_into()?;
    
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = rt.enter();
        listener.set_nonblocking(true).unwrap();
        let listener = UnixListener::from_std(listener).unwrap();
    
        rt.block_on(main_loop(listener)).unwrap();

        Ok(())
    });
}

pub fn maybe_start() -> anyhow::Result<SidecarTransport> {
    let liaison = DefaultLiason::ipc_shared();
    let stdout = File::options()
        .append(true)
        .truncate(false)
        .create(true)
        .write(true)
        .open("/tmp/sidecar.stdout.log")?;

    let stderr = File::options()
        .append(true)
        .truncate(false)
        .create(true)
        .write(true)
        .open("/tmp/sidecar.stderr.log")?;
    if let Some(listener) = liaison.attempt_listen()? {
        unsafe { spawn_worker::SpawnWorker::new() }
            .stdin(Stdio::Null)
            .process_name("datadog-ipc-sidecar")
            .stderr(Stdio::Fd(stderr.into()))
            .stdout(Stdio::Fd(stdout.into()))
            .pass_fd(listener)
            .daemonize(true)
            .target(entrypoint!(ipc_agent_entrypoint))
            .spawn()?;
    }

    let connection = liaison.connect_to_server()?;

    //LEAK! to ensure the connection will remain open for the duration of the process
    connection.try_clone().map(|s| s.into_raw_fd())?;
    connection.set_nonblocking(true)?;

    Ok(SidecarTransport::new(connection))
}
