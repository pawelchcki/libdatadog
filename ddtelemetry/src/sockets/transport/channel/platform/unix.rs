use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io,
    os::unix::{
        net::UnixStream as StdUnixStream,
        prelude::{FromRawFd, IntoRawFd, RawFd},
    },
    sync::{Arc, Mutex, RwLock},
    task::Poll,
};

use sendfd::{RecvWithFd, SendWithFd};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};
use tokio_serde::{formats::MessagePack, Serializer};

use crate::{
    fork::Forkable,
    sockets::transport::handles::{
        BetterHandle, HandlesMove, HandlesProvider, HandlesReceive, HandlesTransfer,
    },
};

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
const MAX_FDS: usize = 20;

#[derive(Debug)]
pub struct Channel {
    inner: StdUnixStream,
}

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    item: Item,
    acked_handles: Vec<RawFd>,
    pid: libc::pid_t,
}

impl<T> HandlesMove for Message<T>
where
    T: HandlesMove,
{
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransfer,
    {
        self.item.move_handles(mover)
    }
}

impl<T> HandlesReceive for Message<T>
where
    T: HandlesReceive,
{
    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesProvider,
    {
        self.item.receive_handles(provider)
    }
}

impl From<Channel> for PlatformHandle {
    fn from(c: Channel) -> Self {
        unsafe { PlatformHandle::from_raw_fd(c.inner.into_raw_fd()) }
    }
}

impl Channel {
    pub fn pair() -> io::Result<(Self, Forkable<Self>)> {
        let (local, remote) = StdUnixStream::pair()?;

        Ok((
            Self::from_std(local),
            Forkable::mark_as(Self::from_std(remote)),
        ))
    }

    fn from_std(s: StdUnixStream) -> Self {
        Self { inner: s }
    }
}

#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: UnixStream,
    pub metadata: ChannelMetadata,
    handles_to_close: Vec<PlatformHandle>,
}

#[derive(Debug, Clone)]
pub struct ChannelMetadata {
    fds_to_send: Arc<Mutex<Vec<PlatformHandle>>>,
    fds_received: Arc<Mutex<VecDeque<RawFd>>>,
    fds_acked: Arc<Mutex<Vec<RawFd>>>,
    fds_to_close: Arc<Mutex<BTreeMap<RawFd, PlatformHandle>>>,
    pid: libc::pid_t, // must always be set to current Process ID
}

impl HandlesTransfer for &mut ChannelMetadata {
    type Error = io::Error;

    fn move_handle<'h, T>(self, handle: BetterHandle<T>) -> Result<(), Self::Error> {
        self.queue_for_sending(handle.into());

        Ok(())
    }
}

impl ChannelMetadata {
    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: HandlesReceive,
    {
        { // close all open file desriptors that were ACKed by the other party
            let mut fds_to_close = self.fds_to_close.lock().unwrap();
            let mut fds_to_close: Vec<PlatformHandle> = message
                .acked_handles
                .into_iter()
                .flat_map(|fd| fds_to_close.remove(&fd))
                .collect();

            // if ACK came from the same PID, it means there is a duplicate PlatformHandle instance in the same
            // process. Thus we should leak the handles
            if message.pid == self.pid {
                fds_to_close.iter_mut().for_each(|h| {
                    h.leak();
                });
            }
        }
        let mut item = message.item;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>  where T: HandlesMove {
        item.move_handles(&mut *self)?;

        let message = Message {
            item,
            acked_handles: self.fds_acked.lock().unwrap().drain(..).collect(),
            pid: self.pid,
        };

        Ok(message)
    }

    fn defer_close_handles(&mut self, handles: Vec<PlatformHandle>) {
        let handles = handles.into_iter().map(|h| (h.fd, h));
        self.fds_to_close.lock().unwrap().extend(handles);
    }

    fn queue_for_sending(&mut self, handle: PlatformHandle) {
        self.fds_to_send.lock().unwrap().push(handle)
    }

    fn find_handle(&mut self, hint: &PlatformHandle) -> Option<PlatformHandle> {
        if hint.fd < 0 {
            return Some(hint.clone());
        }

        let fd = self.fds_received.lock().unwrap().pop_front();

        match fd {
            Some(fd) => Some(unsafe { PlatformHandle::from_raw_fd(fd) }),
            None => None,
        }
    }
}

impl HandlesProvider for &mut ChannelMetadata {
    type Error = io::Error;

    fn provide_handle(self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error> {
        self.find_handle(hint).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("can't provide expected handle for hint: {:?}", hint),
            )
        })
    }
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        let fd = value.inner;
        fd.set_nonblocking(true)?;
        Ok(AsyncChannel {
            inner: UnixStream::from_std(fd)?,
            metadata: ChannelMetadata {
                fds_to_send: Arc::new(Mutex::new(vec![])),
                fds_received: Arc::new(Mutex::new(vec![].into())),
                fds_to_close: Arc::new(Mutex::new(BTreeMap::new())),
                fds_acked: Arc::new(Mutex::new(vec![])),
                pid: unsafe { libc::getpid() },
            },
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
        let handles: Vec<PlatformHandle> = {
            let mut v = project.metadata.fds_to_send.lock().unwrap();
            let len = MAX_FDS.min(v.len());
            v.drain(..len)
                .filter(|h| h.inner.read().unwrap().fd >= 0)
                .collect()
        };

        if handles.len() > 0 {
            let fds: Vec<RawFd> = handles.iter().map(|h| h.inner.read().unwrap().fd).collect();
            match project.inner.send_with_fd(buf, &fds) {
                Ok(sent) => {
                    project.metadata.defer_close_handles(handles);
                    Poll::Ready(Ok(sent))
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    project
                        .metadata
                        .fds_to_send
                        .lock()
                        .unwrap()
                        .extend(handles.into_iter());
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
                        .fds_received
                        .lock()
                        .unwrap()
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlatformHandle {
    fd: RawFd, // Just an fd number to be used as reference, not for accessing actuall fd
    #[serde(skip)]
    inner: Arc<RwLock<PlatformHandleInner>>,
}

impl PlatformHandle {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    ///
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        let inner = Arc::new(RwLock::new(PlatformHandleInner { fd }));
        Self { fd, inner }
    }

    pub fn leak(&mut self) -> RawFd {
        self.inner.write().unwrap().leak()
    }
}

impl PlatformHandle {
    fn try_into_rawfd(self: PlatformHandle) -> Result<RawFd, io::Error> {
        let fd = self.inner.read().unwrap().fd;
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "attempting to unwrap unitialized platform handle (fd: {} ({}))",
                    self.fd, fd
                ),
            ));
        }
        Ok(self.inner.write().unwrap().leak())
    }
}

#[derive(Debug)]
struct PlatformHandleInner {
    fd: RawFd,
}

impl PlatformHandleInner {
    fn leak(&mut self) -> RawFd {
        let fd = self.fd;
        self.fd = -1; // prevend FD from being closed on drop
        fd
    }
}

impl Default for PlatformHandleInner {
    fn default() -> Self {
        Self { fd: -1 }
    }
}

impl Default for PlatformHandle {
    fn default() -> Self {
        let inner = Arc::from(RwLock::new(PlatformHandleInner::default()));
        let fd = inner.read().unwrap().fd;
        Self { fd, inner }
    }
}

impl Drop for PlatformHandleInner {
    fn drop(&mut self) {
        if self.fd > 0 {
            unsafe {
                //TODO handle libc close errors
                libc::close(self.fd);
            }
        }
    }
}

impl From<File> for PlatformHandle {
    fn from(f: File) -> Self {
        {
            unsafe { PlatformHandle::from_raw_fd(f.into_raw_fd()) }
        }
    }
}

impl TryFrom<PlatformHandle> for File {
    type Error = io::Error;

    fn try_from(handle: PlatformHandle) -> Result<Self, Self::Error> {
        let fd = handle.try_into_rawfd()?;

        // Safety: handle try_unwrap_inner will ensure returned handle is initialized and only owned once
        // Safety: all callers should  ensure handle is a file handle
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
