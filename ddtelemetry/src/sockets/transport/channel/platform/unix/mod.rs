use std::{
    borrow::Borrow,
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::{self, Write},
    os::unix::{
        net::UnixStream as StdUnixStream,
        prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
    },
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    task::Poll,
};

#[cfg(test)]
mod tests;

use sendfd::{RecvWithFd, SendWithFd};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};

use crate::{
    fork::{getpid, ForkSafe},
    sockets::transport::handles::{
         HandlesTransport, TransferHandles,
    },
};

mod platform_handle;
pub use platform_handle::*;

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
pub const MAX_FDS: usize = 20;

#[derive(Debug, Clone)]
pub struct Channel {
    inner: PlatformHandle<StdUnixStream>,
}

impl<T> ForkSafe for PlatformHandle<T> {}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut h = unsafe { self.inner.as_borrowed()? };
        h.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut h = unsafe { self.inner.as_borrowed()? };

        h.flush()
    }
}

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    pub item: Item,
    pub acked_handles: Vec<RawFd>,
    pub pid: libc::pid_t,
}

impl<Item> Message<Item> {
    pub fn ref_item<'a>(&'a self) -> &'a Item {
        &self.item
    }
}

impl<T> TransferHandles for Message<T>
where
    T: TransferHandles,
{
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransport,
    {
        self.item.move_handles(mover)
    }

    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesTransport,
    {
        self.item.receive_handles(provider)
    }
}

impl From<Channel> for PlatformHandle<StdUnixStream> {
    fn from(c: Channel) -> Self {
        c.inner
    }
}

#[derive(Debug)]
pub struct ChannelPair(PlatformHandle<StdUnixStream>, PlatformHandle<StdUnixStream>);

impl ChannelPair {
    pub fn local(&self) -> Result<Channel, std::io::Error> {
        unsafe {
            self.1.try_steal().unwrap_or_default(); 
            Ok(Channel::from(self.0.try_steal()?))
        }
    }
    pub fn remote(&self) -> Result<Channel, std::io::Error> {
        unsafe {
            self.0.try_steal().unwrap_or_default(); 
            Ok(Channel::from(self.1.try_steal()?))
        }
    }

    pub fn pair(self) -> Result<(Channel, Channel), std::io::Error> {
        unsafe {
            Ok((Channel::from(self.0.try_steal()?),
            Channel::from(self.1.try_steal()?)))
        }
    }
}

impl<T> From<T> for PlatformHandle<T> where T: IntoRawFd {
    fn from(h: T) -> Self {
        unsafe { PlatformHandle::from_raw_fd(h.into_raw_fd()) }
    }
}

impl ForkSafe for &ChannelPair {}

impl Channel {
    pub fn pair() -> io::Result<ChannelPair> {
        let (local, remote) = StdUnixStream::pair()?;

        unsafe {
            Ok(ChannelPair(
                PlatformHandle::from_raw_fd(local.into_raw_fd()),
                PlatformHandle::from_raw_fd(remote.into_raw_fd()),
            ))
        }
    }
}

impl From<PlatformHandle<StdUnixStream>> for Channel {
    fn from(h: PlatformHandle<StdUnixStream>) -> Self {
        Channel { inner: h }
    }
}



#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: UnixStream,
    pub metadata: Arc<Mutex<ChannelMetadata>>,
    handles_to_close: Vec<PlatformHandle<RawFd>>,
}

#[derive(Debug, Clone)]
pub struct ChannelMetadata {
    fds_to_send: Vec<PlatformHandle<RawFd>>,
    fds_received: VecDeque<RawFd>,
    fds_acked: Vec<RawFd>,
    fds_to_close: BTreeMap<RawFd, PlatformHandle<RawFd>>,
    pid: libc::pid_t, // must always be set to current Process ID
}

impl Default for ChannelMetadata {
    fn default() -> Self {
        Self {
            fds_to_send: Default::default(),
            fds_received: Default::default(),
            fds_acked: Default::default(),
            fds_to_close: Default::default(),
            pid: getpid(),
        }
    }
}

impl HandlesTransport for &mut ChannelMetadata {
    type Error = io::Error;

    fn move_handle<'h, T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        self.enqueue_for_sending(handle);

        Ok(())
    }

    fn provide_handle<T>(self, hint: &PlatformHandle<T>) -> Result<PlatformHandle<T>, Self::Error> {
        self.find_handle(hint).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("can't provide expected handle for hint: {}", hint.as_raw_fd()),
            )
        })
    }
}

impl ChannelMetadata {
    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        {
            // close all open file desriptors that were ACKed by the other party
            let fds_to_close: Vec<PlatformHandle<RawFd>> = message
                .acked_handles
                .into_iter()
                .flat_map(|fd| self.fds_to_close.remove(&fd))
                .collect();

            // if ACK came from the same PID, it means there is a duplicate PlatformHandle instance in the same
            // process. Thus we should leak the handles allowing other PlatformHandle's to safely close
            if message.pid == self.pid {
                for h in fds_to_close.into_iter() {
                    h.try_leak()?;
                }
            }
        }
        let mut item = message.item;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        item.move_handles(&mut *self)?;

        let message = Message {
            item,
            acked_handles: self.fds_acked.drain(..).collect(),
            pid: self.pid,
        };

        Ok(message)
    }

    pub(crate) fn defer_close_handles<T>(&mut self, handles: Vec<PlatformHandle<T>>) {
        let handles = handles.into_iter().map(|h| (h.as_raw_fd(), h.to_rawfd_type() ));
        self.fds_to_close.extend(handles);
    }

    pub(crate) fn enqueue_for_sending<T>(&mut self, handle: PlatformHandle<T>) {
        self.fds_to_send.push(handle.to_rawfd_type())
    }

    pub(crate) fn reenqueue_for_sending(&mut self, mut handles: Vec<PlatformHandle<RawFd>>) {
        handles.extend(self.fds_to_send.drain(..));
        self.fds_to_send = handles;
    }

    pub(crate) fn drain_to_send(&mut self) -> Vec<PlatformHandle<RawFd>> {
        let drain = self
            .fds_to_send
            .drain(..)
            .filter_map(|h| h.try_claim().ok());

        let mut cnt: i32 = MAX_FDS.try_into().unwrap_or(i32::MAX);

        let (to_send, leftover) = drain.partition(|_| {
            cnt -= 1;
            cnt >= 0
        });
        self.reenqueue_for_sending(leftover);

        to_send
    }

    pub(crate) fn find_handle<T>(&mut self, hint: &PlatformHandle<T>) -> Option<PlatformHandle<T>> {
        if hint.as_raw_fd() < 0 {
            return Some(hint.clone());
        }

        let fd = self.fds_received.pop_front();

        match fd {
            Some(fd) => Some(unsafe { PlatformHandle::from_raw_fd(fd) }),
            None => None,
        }
    }
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        let fd = value.inner.try_unwrap_into()?;

        fd.set_nonblocking(true)?;
        Ok(AsyncChannel {
            inner: UnixStream::from_std(fd)?,
            metadata: Arc::new(Mutex::new(ChannelMetadata::default())),
            handles_to_close: vec![],
        })
    }
}

impl AsyncWrite for AsyncChannel {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let project = self.project();
        let handles: Vec<PlatformHandle<RawFd>> = project.metadata.lock().unwrap().drain_to_send();

        if handles.len() > 0 {
            let fds: Vec<RawFd> = handles.iter().map(AsRawFd::as_raw_fd).collect();
            match project.inner.send_with_fd(buf, &fds) {
                Ok(sent) => {
                    project
                        .metadata
                        .lock()
                        .unwrap()
                        .defer_close_handles(handles);
                    Poll::Ready(Ok(sent))
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    project
                        .metadata
                        .lock()
                        .unwrap()
                        .reenqueue_for_sending(handles);
                    project.inner.poll_write_ready(cx).map_ok(|_| 0)
                }
                Err(err) => Poll::Ready(Err(err)),
            }
        } else {
            project.inner.poll_write(cx, buf)
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}

impl AsyncRead for AsyncChannel {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let project = self.project();
        let mut fds = [0; MAX_FDS];

        unsafe {
            let b = &mut *(buf.unfilled_mut() as *mut [std::mem::MaybeUninit<u8>] as *mut [u8]);
            match project.inner.recv_with_fd(b, &mut fds) {
                Ok((bytes_received, descriptors_received)) => {
                    let fds = fds[..descriptors_received].to_vec();
                    project
                        .metadata
                        .lock()
                        .unwrap()
                        .fds_received
                        .append(&mut fds.into());

                    buf.assume_init(bytes_received);
                    buf.advance(bytes_received);

                    Poll::Ready(Ok(()))
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    project.inner.poll_read_ready(cx)
                }
                Err(err) => Poll::Ready(Err(err)),
            }
        }
    }
}
