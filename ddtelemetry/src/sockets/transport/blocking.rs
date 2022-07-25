use std::{
    io::{self},
    pin::Pin,
    sync::{atomic::AtomicU64, Arc},
    time::SystemTime,
};

use bytes::BytesMut;
use serde::Serialize;
use tarpc::{context, trace};
use tokio_serde::{formats::SymmetricalMessagePack, Serializer};
use tokio_util::codec::{Encoder, LengthDelimitedCodec};

use super::{
    channel::{Channel, Message},
    handles::{HandlesTransport, TransferHandles},
};

pub struct BlockingTransport<Item> {
    channel: Channel,
    pid: libc::pid_t,
    requests_id: Arc<AtomicU64>,
    codec: FramedBlocking<Message<ClientMessage<Item>>>,
}

impl<Item> Clone for BlockingTransport<Item> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            pid: self.pid,
            requests_id: self.requests_id.clone(),
            codec: self.codec.clone(),
        }
    }
}

impl<Item> From<Channel> for BlockingTransport<Item> {
    fn from(c: Channel) -> Self {
        let pid = unsafe { libc::getpid() };
        BlockingTransport {
            channel: c,
            pid,
            requests_id: Arc::from(AtomicU64::new(0)),
            codec: FramedBlocking::default(),
        }
    }
}

pub struct FramedBlocking<Item> {
    len: LengthDelimitedCodec,
    codec: Pin<Box<SymmetricalMessagePack<Item>>>,
}

impl<Item> Clone for FramedBlocking<Item> {
    fn clone(&self) -> Self {
        Self {
            len: self.len.clone(),
            codec: Box::pin(Default::default()),
        }
    }
}

impl<Item> Default for FramedBlocking<Item> {
    fn default() -> Self {
        Self {
            len: Default::default(),
            codec: Box::pin(Default::default()),
        }
    }
}

impl<Item> Encoder<Item> for FramedBlocking<Item>
where
    Item: Serialize,
{
    type Error = io::Error;
    fn encode(&mut self, item: Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let data = self.codec.as_mut().serialize(&item)?;

        self.len.encode(data, dst)
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Request<T> {
    /// Trace context, deadline, and other cross-cutting concerns.
    pub context: context::Context,
    /// Uniquely identifies the request across all requests sent over a single channel.
    pub id: u64,
    /// The request body.
    pub message: T,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum ClientMessage<T> {
    Request(Request<T>),
    Cancel {
        #[serde(default)]
        trace_context: trace::Context,
        /// The ID of the request to cancel.
        request_id: u64,
    },
}

impl<T> TransferHandles for ClientMessage<T>
where
    T: TransferHandles,
{
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        match self {
            ClientMessage::Request(r) => r.move_handles(transport),
            ClientMessage::Cancel {
                trace_context: _,
                request_id: _,
            } => Ok(()),
        }
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        match self {
            ClientMessage::Request(r) => r.receive_handles(transport),
            ClientMessage::Cancel { trace_context: _, request_id: _ } => todo!(),
        }
    }
}

impl<T> TransferHandles for Request<T>
where
    T: TransferHandles,
{
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.message.move_handles(transport)
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.message.receive_handles(transport)
    }
}

impl<Item> BlockingTransport<Item>
where
    Item: Serialize + TransferHandles,
{
    fn new_client_message(&self, item: Item, deadline: Option<SystemTime>) -> ClientMessage<Item> {
        let mut context = context::current();

        if let Some(deadline) = deadline {
            context.deadline = deadline;
        }

        let request_id = self
            .requests_id
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);

        ClientMessage::Request(Request {
            context,
            id: request_id,
            message: item,
        })
    }

    pub fn send_ignore_response(&mut self, item: Item) -> Result<(), io::Error> {
        let req = self.new_client_message(item, Some(SystemTime::UNIX_EPOCH));

        let msg = self.channel.metadata.create_message(req)?;

        let mut buf = BytesMut::new();
        self.codec.encode(msg, &mut buf)?;

        if buf.len() > 65000 {
            //TODO if message is greater that 65k (PIPE_BUF on modern Linuxes) the messages will be interleaved
            // with other users of the channel
        }

        self.channel.send_message(&buf)
    }
}
