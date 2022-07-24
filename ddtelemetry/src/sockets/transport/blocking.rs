use std::{
    io::{self, Read, Write},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, AtomicUsize},
        Arc,
    },
    time::SystemTime, os::unix::{prelude::{AsRawFd, FromRawFd}, net::UnixStream},
};

use bytes::{BufMut, Bytes, BytesMut};
use serde::Serialize;
use tarpc::{context, trace};
use tokio_serde::{formats::SymmetricalMessagePack, Serializer};
use tokio_util::codec::{Encoder, LengthDelimitedCodec};

use super::{
    channel::{Channel, Message},
    handles::TransferHandles,
};

pub struct BlockingChannel<Item> {
    channel: Channel,
    pid: libc::pid_t,
    requests_id: Arc<AtomicU64>,
    codec: FramedBlocking<Message<ClientMessage<Item>>>,
}

impl<Item> From<Channel> for BlockingChannel<Item> {
    fn from(c: Channel) -> Self {
        let pid = unsafe { libc::getpid() };
        BlockingChannel {
            channel: c,
            pid: pid,
            requests_id: Arc::from(AtomicU64::new(0)),
            codec: FramedBlocking::default()
        }
    }
}

pub struct FramedBlocking<Item> {
    len: LengthDelimitedCodec,
    codec: Pin<Box<SymmetricalMessagePack<Item>>>,
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
    fn move_handles<Transport: super::handles::HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }

    fn receive_handles<Transport: super::handles::HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        Ok(())
    }
}

impl<Item> BlockingChannel<Item>   where
Item: Serialize + TransferHandles {
    pub fn send_and_forget(&mut self, req: Item) -> Result<(), io::Error>
    {
        let mut context = context::current();
        context.deadline = SystemTime::UNIX_EPOCH;
        let request_id = self
            .requests_id
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let req = ClientMessage::Request(Request {
            context: context,
            id: request_id,
            message: req,
        });

        let msg = self.create_message(req)?;
        let mut buf = BytesMut::new();
        self.codec.encode(msg, &mut buf)?;

        self.channel.write_all(&buf)
    }

    pub fn read_to_dev_null(&mut self) -> Result<(), io::Error>
    {
        // let fd = self.channel.as_raw_fd();

        // UnixStream::from_raw_fd(fd)
        // self.channel.read

        Ok(())
    }

    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        Ok(message.item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        let message = Message {
            item,
            acked_handles: vec![],
            pid: self.pid,
        };

        Ok(message)
    }
}
