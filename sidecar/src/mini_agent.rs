use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::io::Write;
use std::os::fd::IntoRawFd;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use datadog_trace_protobuf::pb::{AgentPayload, TracerPayload};
use datadog_trace_protobuf::prost::Message;
use ddcommon::HttpClient;
use hyper::service::Service;
use hyper::{Body, Method, Request, Response, Server, StatusCode};

use tokio::net::UnixListener;
use tokio::sync::mpsc::{Sender, Receiver};

use crate::connections::UnixListenerTracked;
use crate::data::v04::{self};
use crate::tracing::uploader::Uploader;

// Example traced app: go install github.com/DataDog/trace-examples/go/heartbeat@latest
#[derive(Debug, Clone)]
struct V04Handler {
    builder: v04::AssemblerBuilder,
    payload_sender: Sender<Box<TracerPayload>>,
}

impl V04Handler {
    fn new(tx: Sender<Box<TracerPayload>>) -> Self {
        Self {
            builder: Default::default(),
            payload_sender: tx,
        }
    }
}

#[derive(Debug)]
struct MiniAgent {
    v04_handler: V04Handler,
}

impl MiniAgent {
    fn new(tx: Sender<Box<TracerPayload>>) -> Self {
        Self {
            v04_handler: V04Handler::new(tx),
        }
    }
}

impl Service<Request<Body>> for MiniAgent {
    type Response = Response<Body>;
    type Error = anyhow::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        match (req.method(), req.uri().path()) {
            // exit, shutting down the subprocess process.
            // (&Method::GET, "/exit") => {
            //     std::process::exit(0);

            // }
            // node.js does put while Go does POST whoa
            (&Method::POST | &Method::PUT, "/v0.4/traces") => {
                let handler = self.v04_handler.clone();
                Box::pin(async move { handler.handle(req).await })
            }

            // Return the 404 Not Found for other routes.
            _ => Box::pin(async move {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }),
        }
    }
}

impl V04Handler {
    async fn handle(&self, mut req: Request<Body>) -> anyhow::Result<Response<Body>> {
        let body = hyper::body::to_bytes(req.body_mut()).await?;
        let src: v04::Payload = rmp_serde::from_slice(&body)?;

        let payload = self
            .builder
            .with_headers(req.headers())
            .assemble_payload(src);

        self.payload_sender.send(Box::new(payload)).await?;

        Ok(Response::default())
    }
}

struct MiniAgentSpawner {
    payload_sender: Sender<Box<TracerPayload>>,
}

impl<'t, Target> Service<&'t Target> for MiniAgentSpawner {
    type Response = MiniAgent;
    type Error = Box<dyn Error + Send + Sync>;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: &'t Target) -> Self::Future {
        let agent = MiniAgent::new(self.payload_sender.clone());

        Box::pin(async { Ok(agent) })
    }
}

pub(crate) async fn main(listener: UnixListener) -> anyhow::Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Box<TracerPayload>>(10);

    let uploader = Uploader::init(&crate::config::Config::init());
    tokio::spawn(uploader.into_event_loop(rx));
    let listener = UnixListenerTracked::from(listener);
    let watcher = listener.watch();
    let server = Server::builder(listener).serve(MiniAgentSpawner { payload_sender: tx });
    tokio::select! {
        // if there are no connections for 1 second, exit the main loop
        _ = watcher.wait_for_no_instances(Duration::from_secs(1)) => {
            Ok(())
        }
        res = server => {
            res?;
            Ok(())
        }
    }
}

use std::{
    os::{fd::AsRawFd, unix::net::UnixListener as StdUnixListener},
    path::PathBuf,
};

use ddtelemetry::ipc::setup::Liaison;
use spawn_worker::{entrypoint, Stdio};

#[no_mangle]
pub extern "C" fn mini_agent_entrypoint() {
    if let Err(e) = mini_agent() {
        eprintln!("exiting err: {}", e)
    }
}

fn mini_agent() -> anyhow::Result<()> {
    if let Ok(fd) = spawn_worker::recv_passed_fd() {
        let listener = StdUnixListener::try_from(fd)?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _rt_guard = rt.enter();
        listener.set_nonblocking(true).unwrap();
        let listener = UnixListener::from_std(listener)?;

        let server_future = main(listener);

        rt.block_on(server_future)?;
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) unsafe fn maybe_start() -> anyhow::Result<PathBuf> {
    let liaison = ddtelemetry::ipc::setup::SharedDirLiaison::new(
        std::env::temp_dir().join("libdatadog-mini-agent"),
    );
    if let Some(listener) = liaison.attempt_listen()? {
        spawn_worker::SpawnWorker::new()
            .process_name("datadog-mini-agent-sidecar")
            .stdin(Stdio::Null)
            .stderr(Stdio::Inherit)
            .stdout(Stdio::Inherit)
            .pass_fd(listener)
            .daemonize(true)
            .target(entrypoint!(mini_agent_entrypoint))
            .spawn()?;
    };

    // TODO: temporary hack - connect to socket and leak it
    // this should lead to sidecar being up as long as the processes that attempted to connect to it
    let con = liaison.connect_to_server()?;
    con.into_raw_fd(); // LEAK!

    Ok(liaison.path().to_path_buf())
}
