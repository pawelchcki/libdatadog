use std::{os::unix::{net::UnixStream, prelude::{RawFd, AsRawFd}}, io, pin::Pin, sync::mpsc, error::Error};

use futures::Sink;
use serde::{Serialize, Deserialize, Deserializer, };
use tokio_serde::Serializer;


pub mod channel;
pub mod handles;

pub trait AllReqs<'de>: serde::Serialize + serde::Deserialize<'de> {}

#[derive(Serialize, Deserialize)]
struct ExampleData {
    #[serde(skip_serializing, skip_deserializing)]
    channel: Option<channel::Channel>,
    string: String
}

impl handles::SerializeHandles for ExampleData {
    fn serialize_handles<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: handles::HandlesSerializer {
        self.channel.serialize_handles(serializer)
    }
}

pub trait AllSerReqs: serde::Serialize {

}

pub struct Transport<Codec> {
    // TODO: ipc_channel assumes only a single directional communication, while we need bidirectionality
    // consider refactoring ipc_channel implementation to allow bi-directional transport
    channel: channel::AsyncChannel,
    codec: Codec
}


fn send<'a, S: SendWithFd, D: AllSerReqs + 'a, Ser: Serializer<D>>(serializer: Pin<&mut Ser>, sender: S, data: D) -> io::Result<usize>{
    // serializer.serialize(data);
    Ok(0)
}

impl<SinkItem, Codec> Sink<SinkItem> for Transport<Codec> where SinkItem: Serialize, Codec: Serializer<SinkItem>{
    type Error = anyhow::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        todo!()
    }
}

pub trait SendWithFd {
    /// Send the bytes and the file descriptors.
    fn send_with_fd(&self, bytes: &[u8], fds: &[RawFd]) -> io::Result<usize>;
}

/// An extension trait that enables receiving associated file descriptors along with the data.
pub trait RecvWithFd {
    /// Receive the bytes and the file descriptors.
    ///
    /// The bytes and the file descriptors are received into the corresponding buffers.
    fn recv_with_fd(&self, bytes: &mut [u8], fds: &mut [RawFd]) -> io::Result<(usize, usize)>;
}
