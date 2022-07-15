use std::{
    collections::VecDeque,
    fmt::Display,
    fs::File,
    io,
    os::unix::{
        net::UnixStream as StdUnixStream,
        prelude::{FromRawFd, IntoRawFd, RawFd},
    },
    sync::{Arc, Mutex},
    task::Poll,
};

use sendfd::{RecvWithFd, SendWithFd};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};

use crate::{
    fork::Forkable,
    sockets::transport::handles::{BetterHandle, HandlesProvider, HandlesTransfer},
};

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
const MAX_FDS: usize = 20;

#[derive(Debug)]
pub struct Channel {
    inner: StdUnixStream,
}

impl From<Channel> for PlatformHandle {
    fn from(c: Channel) -> Self {
        unsafe { PlatformHandle::from_raw_fd(c.inner.into_raw_fd()) }
    }
}

impl Channel {
    pub fn pair() -> std::io::Result<(Self, Forkable<Self>)> {
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
    metadata: Arc<ChannelMetadata>,
    handles_to_close: Vec<PlatformHandle>,
}

impl AsyncChannel {
    pub fn share_metadata(&self) -> Arc<ChannelMetadata> {
        Arc::clone(&self.metadata)
    }
}

#[derive(Debug)]
pub struct ClosableFd {
    fd: RawFd,
}

impl Drop for ClosableFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl From<RawFd> for ClosableFd {
    fn from(fd: RawFd) -> Self {
        Self { fd }
    }
}

#[derive(Debug)]
pub struct ChannelMetadata {
    fds_to_send: Mutex<Vec<PlatformHandle>>,
    fds_received: Mutex<VecDeque<RawFd>>,
}

impl HandlesTransfer for &mut Arc<ChannelMetadata> {
    type Error = std::io::Error;

    fn move_handle<'h, T>(self, handle: BetterHandle<T>) -> Result<(), Self::Error> {
        self.fds_to_send.lock().unwrap().push(handle.into());
        Ok(())
    }
}

impl HandlesProvider for &mut Arc<ChannelMetadata> {
    type Error = std::io::Error;

    fn provide_handle(&self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error> {
        if hint.fd > 0 {
            self.fds_received
                .lock()
                .unwrap()
                .pop_front()
                .map(|fd| unsafe { PlatformHandle::from_raw_fd(fd) })
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("can't provide expected handle for hint: {:?}", hint),
                    )
                })
        } else {
            Ok(hint.clone())
        }
    }
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = std::io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        let fd = value.inner;
        fd.set_nonblocking(true)?;
        Ok(AsyncChannel {
            inner: UnixStream::from_std(fd)?,
            metadata: Arc::new(ChannelMetadata {
                fds_to_send: Mutex::new(vec![]),
                fds_received: Mutex::new(vec![].into()),
            }),
            handles_to_close: vec![],
        })
    }
}

impl AsyncWrite for AsyncChannel {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let project = self.project();
        let mut handles: Vec<PlatformHandle> = {
            let mut v = project.metadata.fds_to_send.lock().unwrap();
            let len = MAX_FDS.min(v.len());
            v.drain(..len).filter(|h| h.inner.fd >= 0).collect()
        };

        if handles.len() > 0 {
            let fds: Vec<RawFd> = handles.iter().map(|h| h.inner.fd).collect();
            match project.inner.send_with_fd(buf, &fds) {
                Ok(sent) => {
                    //TODO: on linux fds can be closed immediately after being sent - however on OSX, we need to wait until they are accepted by the other party
                    // For now lets leak FDs indefinitely
                    project.handles_to_close.append(&mut handles);
                    Poll::Ready(Ok(sent))
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    //TODO reinject fds to send
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
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}

impl AsyncRead for AsyncChannel {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
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
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
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
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    ///          have exclusive ownership of the file descriptor
    ///
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        let inner = Arc::new(PlatformHandleInner { fd });
        Self { fd, inner }
    }
}

impl PlatformHandle {
    fn try_into_rawfd(self: PlatformHandle) -> Result<RawFd, io::Error> {
        if self.inner.fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "attempting to unwrap unitialized platform handle (fd: {} ({}))",
                    self.fd, self.inner.fd
                ),
            ));
        }

        match Arc::try_unwrap(self.inner) {
            Ok(mut inner) => {
                let fd = inner.fd;
                inner.fd = -1; // prevend FD from being closed on drop
                Ok(fd)
            }
            Err(inner) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "attempting to unwrap a non unique platform handle (fd: {}, copies: {})",
                    inner.fd,
                    Arc::strong_count(&inner)
                ),
            )),
        }
    }
}

#[derive(Debug)]
struct PlatformHandleInner {
    fd: RawFd,
}

impl Default for PlatformHandleInner {
    fn default() -> Self {
        Self { fd: -1 }
    }
}

impl Default for PlatformHandle {
    fn default() -> Self {
        let inner = Arc::from(PlatformHandleInner::default());
        let fd = inner.fd;
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
            unsafe {
                PlatformHandle::from_raw_fd(f.into_raw_fd())
            }
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
