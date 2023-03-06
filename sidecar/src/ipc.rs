use std::{
    fs::File,
    future::{ready, Pending, Ready},
    io::Read,
    sync::{atomic::AtomicU32, Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use datadog_trace_protobuf::pb::TracerPayload;
use ddtelemetry::{
    ipc::{
        platform::{AsyncChannel, PlatformHandle},
        transport::{blocking::BlockingTransport, Transport},
    },
    tarpc::{self, context::Context, server::Channel},
};
use std::os::unix::net::UnixStream as StdUnixStream;
use tokio::{io::AsyncReadExt, net::UnixStream, sync::mpsc::Sender};

use crate::tracing::{
    self,
    trace_events::{self, SpanFinished},
};



#[tarpc::service]
pub trait SidecarInterface {
    async fn notify() -> ();
    async fn span_started(msg: trace_events::SpanStart) -> ();
    async fn segfault(msg: trace_events::SegfaultNotification) -> ();
}

#[derive(Clone)]
pub struct SidecarServer {
    event_tx: tokio::sync::mpsc::Sender<trace_events::Event>,
    req_cnt: Arc<AtomicU32>,
    stored_files: Arc<Mutex<Vec<PlatformHandle<File>>>>,
}

impl SidecarInterface for SidecarServer {
    type NotifyFut = Ready<()>;

    fn notify(self, _: Context) -> Self::NotifyFut {
        ready(())
    }

    type SpanStartedFut = Pending<()>;

    fn span_started(
        self,
        _: tarpc::context::Context,
        mut msg: trace_events::SpanStart,
    ) -> Self::SpanStartedFut {
        let event_tx = self.event_tx.clone();
        let span_finished_template = SpanFinished {
            id: msg.id.clone(),
            trace_id: msg.trace_id.clone(),
            parent_span_id: msg.parent_id.clone(),
            duration: 0,
            time: 0,
        };
        let sock = msg.span_end_socket.take();
        let started_at = msg.time;
        tokio::task::spawn_blocking(move || {
            if let Some(sock) = sock {
                if let Err(err) = finalize_span_via_socket_auto_close(
                    event_tx.clone(),
                    started_at,
                    sock,
                    span_finished_template,
                ) {
                    eprintln!("error finalizing span: {:?}", err)
                }
            };
        });

        let event_tx = self.event_tx.clone();

        tokio::task::spawn(async move {
            event_tx
                .send(trace_events::Event::StartSpan(Box::new(msg)))
                .await
                .ok(); // TODO: check error
        });
        std::future::pending() //don't send back the response
    }

    type SegfaultFut = Pending<()>;

    fn segfault(self,_:tarpc::context::Context,msg:trace_events::SegfaultNotification) -> Self::SegfaultFut {
        println!("SEGFAULT: {:?}", msg);
        std::future::pending() //don't send back the response
    }

    
}

fn finalize_span_via_socket_auto_close(
    sender: Sender<trace_events::Event>,
    started_at: u64,
    fd: PlatformHandle<StdUnixStream>,
    mut span_finished: SpanFinished,
) -> anyhow::Result<()> {
    let mut sock = fd.into_instance()?;

    sock.read(&mut [0; 8]).ok();

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    span_finished.time = now.as_nanos() as u64;

    //TODO: log saturated sub;
    span_finished.duration = (now.as_nanos() as u64)
        .checked_sub(started_at)
        .unwrap_or_default();

    sender.blocking_send(trace_events::Event::SpanFinished(span_finished))?;

    Ok(())
}

impl SidecarServer {
    pub fn new(event_tx: tokio::sync::mpsc::Sender<trace_events::Event>) -> Self {
        Self {
            event_tx,
            req_cnt: Default::default(),
            stored_files: Default::default(),
        }
    }
    pub async fn handle(self, connection: UnixStream) {
        let server = tarpc::server::BaseChannel::new(
            tarpc::server::Config::default(),
            Transport::try_from(AsyncChannel::from(connection)).unwrap(),
        );

        server.execute(self.serve()).await
    }
}

mod handles_impl {
    use ddtelemetry::ipc::handles::{HandlesTransport, TransferHandles};

    use super::{SidecarInterfaceRequest, SidecarInterfaceResponse};

    impl TransferHandles for SidecarInterfaceResponse {
        fn move_handles<Transport: HandlesTransport>(
            &self,
            t: Transport,
        ) -> Result<(), Transport::Error> {
            match self {
                _ => Ok(()),
            }
        }

        fn receive_handles<Transport: HandlesTransport>(
            &mut self,
            t: Transport,
        ) -> Result<(), Transport::Error> {
            match self {
                _ => Ok(()),
            }
        }
    }

    impl TransferHandles for SidecarInterfaceRequest {
        fn move_handles<Transport: HandlesTransport>(
            &self,
            t: Transport,
        ) -> Result<(), Transport::Error> {
            match self {
                SidecarInterfaceRequest::SpanStarted { msg } => msg.move_handles(t),
                _ => Ok(()),
            }
        }

        fn receive_handles<Transport: HandlesTransport>(
            &mut self,
            t: Transport,
        ) -> Result<(), Transport::Error> {
            match self {
                Self::SpanStarted { msg } => msg.receive_handles(t),
                _ => Ok(()),
            }
        }
    }
}

pub struct SidecarTransport {
    transport: BlockingTransport<SidecarInterfaceResponse, SidecarInterfaceRequest>,
}

impl SidecarTransport {
    pub fn new(socket: StdUnixStream) -> Self {
        Self {
            transport: BlockingTransport::from(socket),
        }
    }
    pub fn span_started(&mut self, span_start: trace_events::SpanStart) -> anyhow::Result<()> {
        Ok(self
            .transport
            .send_ignore_response(SidecarInterfaceRequest::SpanStarted { msg: span_start })?)
    }
}
