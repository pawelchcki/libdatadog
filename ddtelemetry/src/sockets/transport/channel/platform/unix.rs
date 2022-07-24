use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::{self, Write},
    ops::DerefMut,
    os::unix::{
        net::UnixStream as StdUnixStream,
        prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
    },
    sync::{
        atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    task::Poll,
};

use lazy_static::__Deref;
use sendfd::{RecvWithFd, SendWithFd};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};
use tokio_serde::{formats::MessagePack, Serializer};

use crate::{
    fork::{getpid, ForkSafe, Forkable},
    sockets::transport::handles::{BetterHandle, HandlesTransport, TransferHandles},
};

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
pub const MAX_FDS: usize = 20;

#[derive(Debug)]
pub struct Channel {
    inner: StdUnixStream,
    drop_guard: DropGuard,
}

#[derive(Debug)]
struct DropGuard {}

impl ForkSafe for Channel {}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        println!("{}: dropping channel", unsafe { libc::getpid() })
    }
}

pub struct ChannelRef {
    fd: RawFd,
}

impl Channel {
    pub fn as_channel_ref(&self) -> ChannelRef {
        ChannelRef {
            fd: self.inner.as_raw_fd(),
        }
    }
}

impl ChannelRef {
    pub fn close(self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl ForkSafe for ChannelRef {}

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

impl From<Channel> for PlatformHandle {
    fn from(c: Channel) -> Self {
        unsafe { PlatformHandle::from_raw_fd(c.inner.into_raw_fd()) }
    }
}

impl Channel {
    pub fn pair() -> io::Result<(Self, Self)> {
        let (local, remote) = StdUnixStream::pair()?;

        Ok((Self::from_std(local), Self::from_std(remote)))
    }

    fn from_std(s: StdUnixStream) -> Self {
        Self {
            inner: s,
            drop_guard: DropGuard {},
        }
    }
}

#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: UnixStream,
    pub metadata: Arc<Mutex<ChannelMetadata>>,
    handles_to_close: Vec<PlatformHandle>,
}

#[derive(Debug, Clone)]
pub struct ChannelMetadata {
    fds_to_send: Vec<PlatformHandle>,
    fds_received: VecDeque<RawFd>,
    fds_acked: Vec<RawFd>,
    fds_to_close: BTreeMap<RawFd, PlatformHandle>,
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

    fn move_handle<'h, T>(self, handle: BetterHandle<T>) -> Result<(), Self::Error> {
        self.enqueue_for_sending(handle.into());

        Ok(())
    }

    fn provide_handle(self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error> {
        self.find_handle(hint).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("can't provide expected handle for hint: {:?}", hint),
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
            let mut fds_to_close: Vec<PlatformHandle> = message
                .acked_handles
                .into_iter()
                .flat_map(|fd| self.fds_to_close.remove(&fd))
                .collect();

            // if ACK came from the same PID, it means there is a duplicate PlatformHandle instance in the same
            // process. Thus we should leak the handles allowing other PlatformHandle's to safely close
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

    pub(crate) fn defer_close_handles(&mut self, handles: Vec<PlatformHandle>) {
        let handles = handles.into_iter().map(|h| (h.fd, h));
        self.fds_to_close.extend(handles);
    }

    pub(crate) fn enqueue_for_sending(&mut self, handle: PlatformHandle) {
        self.fds_to_send.push(handle)
    }

    pub(crate) fn reenqueue_for_sending(&mut self, mut handles: Vec<PlatformHandle>) {
        handles.extend(self.fds_to_send.drain(..));
        self.fds_to_send = handles;
    }

    pub(crate) fn drain_to_send(&mut self) -> Vec<PlatformHandle> {
        let drain = self.fds_to_send
            .drain(..)
            .filter_map(|h| h.claim_valid());
            
        
        let mut cnt: i32 = MAX_FDS.try_into().unwrap_or(i32::MAX); 

        let (to_send, leftover) = drain.partition(|_| { cnt-=1; cnt >= 0 });
        self.reenqueue_for_sending(leftover);

        to_send
    }

    pub(crate) fn find_handle(&mut self, hint: &PlatformHandle) -> Option<PlatformHandle> {
        if hint.fd < 0 {
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
        let fd = value.inner;
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
        let handles: Vec<PlatformHandle> = project.metadata.lock().unwrap().drain_to_send();

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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlatformHandle {
    fd: RawFd, // Just an fd number to be used as reference, not for accessing actuall fd
    #[serde(skip)]
    inner: Arc<PlatformHandleInner>,
}

impl PlatformHandle {
    pub fn leak(&self) -> RawFd {
        self.inner.leak()
    }

    /// Creates a new PlatformHandle, transferring the ownership of underlying FD if FD is valid
    pub fn claim_valid(&self) -> Option<PlatformHandle> {
        let handle = unsafe { PlatformHandle::from_raw_fd(self.leak()) };
        if handle.inner.is_valid() {
            Some(handle)
        } else {
            None
        }
    }
}

impl FromRawFd for PlatformHandle {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    ///
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        let inner = Arc::new(PlatformHandleInner::new(fd));
        Self { fd, inner }
    }
}

impl PlatformHandle {
    fn try_into_rawfd(self: PlatformHandle) -> Result<RawFd, io::Error> {
        let fd = self.inner.leak();
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "attempting to unwrap FD from unowned platform handle (fd: {} ({}))",
                    self.fd, fd
                ),
            ));
        }
        Ok(fd)
    }
}

impl AsRawFd for PlatformHandle {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

#[derive(Debug)]
struct PlatformHandleInner {
    fd: AtomicI32,
}

impl PlatformHandleInner {
    pub fn leak(&self) -> RawFd {
        // prevent FD from being closed on drop
        self.fd.swap(-1, Ordering::AcqRel)
    }

    pub fn is_valid(&self) -> bool {
        self.fd.load(Ordering::Acquire) > 0
    }

    pub fn new(fd: RawFd) -> Self {
        Self {
            fd: AtomicI32::new(fd),
        }
    }
}

impl AsRawFd for PlatformHandleInner {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.load(Ordering::Acquire)
    }
}

impl Default for PlatformHandleInner {
    fn default() -> Self {
        Self {
            fd: AtomicI32::new(-1),
        }
    }
}

impl Default for PlatformHandle {
    fn default() -> Self {
        let inner = Arc::from(PlatformHandleInner::default());
        let fd = inner.as_raw_fd();
        Self { fd, inner }
    }
}

impl Drop for PlatformHandleInner {
    fn drop(&mut self) {
        let fd = self.leak();
        if fd > 0 {
            unsafe {
                //TODO handle libc close errors
                libc::close(fd);
            }
        }
    }
}

impl<T> From<T> for PlatformHandle where T: IntoRawFd {
    fn from(p: T) -> Self {
        {
            unsafe { PlatformHandle::from_raw_fd(p.into_raw_fd()) }
        }
    }
}

impl TryFrom<PlatformHandle> for File {
    type Error = io::Error;

    /// # Safety: handle try_unwrap_inner will ensure returned handle is initialized and only owned once
    /// # Safety: all callers should ensure handle is a file handle
    fn try_from(handle: PlatformHandle) -> Result<Self, Self::Error> {
        let fd = handle.try_into_rawfd()?;

        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
