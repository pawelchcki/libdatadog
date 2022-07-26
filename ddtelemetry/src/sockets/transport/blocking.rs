use std::{
    io::{self, Read, Write},
    mem::MaybeUninit,
    pin::Pin,
    sync::{atomic::AtomicU64, Arc},
    time::SystemTime,
};

use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tarpc::{context, trace, Response};
use tokio_serde::{formats::MessagePack, Deserializer, Serializer};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use super::{
    channel::{Channel, Message},
    handles::{HandlesTransport, TransferHandles},
};

pub struct BlockingTransport<IncomingItem, OutgoingItem> {
    channel: Channel,
    pid: libc::pid_t,
    requests_id: Arc<AtomicU64>,
    codec: FramedBlocking<Message<Response<IncomingItem>>, Message<ClientMessage<OutgoingItem>>>,
}

impl<IncomingItem, OutgoingItem> Clone for BlockingTransport<IncomingItem, OutgoingItem> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            pid: self.pid,
            requests_id: self.requests_id.clone(),
            codec: self.codec.clone(),
            // TODO: how to ensure only a single instance of BlockingTransport can read at the same time
            // without using locks :thinking:
        }
    }
}

impl<IncomingItem, OutgoingItem> From<Channel> for BlockingTransport<IncomingItem, OutgoingItem> {
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

pub struct FramedBlocking<IncomingItem, OutgoingItem> {
    len: LengthDelimitedCodec,
    codec: Pin<Box<MessagePack<IncomingItem, OutgoingItem>>>,
}

impl<IncomingItem, OutgoingItem> Clone for FramedBlocking<IncomingItem, OutgoingItem> {
    fn clone(&self) -> Self {
        Self {
            len: self.len.clone(),
            codec: Box::pin(Default::default()),
        }
    }
}

impl<IncomingItem, OutgoingItem> Default for FramedBlocking<IncomingItem, OutgoingItem> {
    fn default() -> Self {
        Self {
            len: Default::default(),
            codec: Box::pin(Default::default()),
        }
    }
}

impl<IncomingItem, OutgoingItem> Encoder<OutgoingItem>
    for FramedBlocking<IncomingItem, OutgoingItem>
where
    OutgoingItem: Serialize,
{
    type Error = io::Error;
    fn encode(&mut self, item: OutgoingItem, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let data = self.codec.as_mut().serialize(&item)?;

        self.len.encode(data, dst)
    }
}

impl<IncomingItem, OutgoingItem> Decoder for FramedBlocking<IncomingItem, OutgoingItem>
where
    IncomingItem: for<'de> Deserialize<'de>,
{
    type Item = IncomingItem;

    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        println!("wtf: {}", src.len());
        match self.len.decode(src)? {
            Some(data) => self.codec.as_mut().deserialize(&data).map(|e| Some(e)),
            None => Ok(None),
        }
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
            ClientMessage::Cancel {
                trace_context: _,
                request_id: _,
            } => todo!(),
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

impl<IncomingItem, OutgoingItem> BlockingTransport<IncomingItem, OutgoingItem>
where
    OutgoingItem: Serialize + TransferHandles,
    IncomingItem: for<'de> Deserialize<'de> + TransferHandles,
{
    fn new_client_message(
        &self,
        item: OutgoingItem,
        deadline: Option<SystemTime>,
    ) -> ClientMessage<OutgoingItem> {
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

    pub fn send_ignore_response(&mut self, item: OutgoingItem) -> Result<(), io::Error> {
        let req = self.new_client_message(item, Some(SystemTime::UNIX_EPOCH));
        self.do_send(req)
    }

    pub fn send(&mut self, item: OutgoingItem) -> Result<Response<IncomingItem>, io::Error> {
        let req = self.new_client_message(item, None);
        self.do_send(req)?;
        self.receive_response()
    }

    fn do_send(&mut self, req: ClientMessage<OutgoingItem>) -> Result<(), io::Error> {
        let msg = self.channel.metadata.create_message(req)?;

        let mut buf = BytesMut::new();
        self.codec.encode(msg, &mut buf)?;

        if buf.len() > 65000 {
            //TODO if message is greater that 65k (PIPE_BUF on modern Linuxes) the messages will be interleaved
            // with other users of the channel
        }

        self.channel.write_all(&buf)
    }

    fn receive_response(&mut self) -> Result<Response<IncomingItem>, io::Error> {
        let mut buf = BytesMut::with_capacity(4000);
        while buf.has_remaining_mut() {
            //TODO consider increasing this limit reading 1 byte might not be optimal
            match self.codec.decode(&mut buf)? {
                Some(message) => {
                    let item = self.channel.metadata.unwrap_message(message)?;
                    return Ok(item);
                }
                None => {
                    let n = unsafe {
                        let dst = buf.chunk_mut();
                        let dst = &mut *(dst as *mut _ as *mut [MaybeUninit<u8>]);
                        let mut buf_window = tokio::io::ReadBuf::uninit(dst);

                        let b = &mut *(buf_window.unfilled_mut()
                            as *mut [std::mem::MaybeUninit<u8>]
                            as *mut [u8]);

                        let n = self.channel.read(b)?;
                        buf_window.assume_init(n);
                        buf_window.advance(n);

                        buf_window.filled().len()
                    };

                    // Safety: This is guaranteed to be the number of initialized (and read)
                    // bytes due to the invariants provided by `ReadBuf::filled`.
                    unsafe {
                        buf.advance_mut(n);
                    }
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            "couldn't read entire item",
        ))
    }
}
