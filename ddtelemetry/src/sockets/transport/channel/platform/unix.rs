use std::{
    os::unix::{
        net::UnixStream as StdUnixStream,
        prelude::{IntoRawFd, RawFd, FromRawFd},
    },
    task::Poll, sync::{Arc, Mutex},
};

use sendfd::{RecvWithFd, SendWithFd};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};

use crate::{fork::Forkable, sockets::transport::handles::{HandlesTransfer, Handle}};

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
const MAX_FDS: usize = 20;

#[derive(Debug)]
pub struct Channel {
    inner: StdUnixStream,
}

impl IntoRawFd for Channel {
    fn into_raw_fd(self) -> std::os::unix::prelude::RawFd {
        self.inner.into_raw_fd()
    }
}

impl From<Channel> for Handle {
    fn from(c: Channel) -> Self {
        Handle::Channel(c.into_raw_fd())
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

    fn next<T>(&mut self) -> Option<T> {
        todo!()
    }
}

#[derive(Debug)]
#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: UnixStream,
    metadata: Arc<ChannelMetadata>,
    fds_to_close: Vec<ClosableFd>,
}

impl AsyncChannel {
    pub fn share_metadata(&self) -> Arc<ChannelMetadata> {
        Arc::clone(&self.metadata)
    }
}

#[derive(Debug)]
pub struct ClosableFd {
    fd: RawFd
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
        Self { fd }    }
}

#[derive(Debug)]
pub struct ChannelMetadata {
    fds_to_send: Mutex<Vec<RawFd>>,
    fds_received: Mutex<Vec<RawFd>>,
}

impl HandlesTransfer for &mut Arc<ChannelMetadata> {
    type Ok = ();

    type Error = std::io::Error;

    fn move_handles<'h>(self, handles: Vec<&'h crate::sockets::transport::handles::Handle>) -> Result<Self::Ok, Self::Error> {
        let mut fds: Vec<RawFd> = handles.clone().into_iter().map(|h| match h {
            crate::sockets::transport::handles::Handle::UnixStream(f) => *f,
            crate::sockets::transport::handles::Handle::Channel(f) => *f,
            crate::sockets::transport::handles::Handle::None => -1,
        }).collect();

        self.fds_to_send.lock().unwrap().append(&mut fds);
        Ok(())
    }

    fn move_handle<'h>(self, handle: &'h crate::sockets::transport::handles::Handle) -> Result<Self::Ok, Self::Error> {
        self.move_handles(vec![handle])
    }
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = std::io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        let fd = value.inner;
        // fd.set_nonblocking(true)?;
        Ok(AsyncChannel {
            inner: UnixStream::from_std(fd)?,
            metadata: Arc::new(ChannelMetadata {
                fds_to_send: Mutex::new(vec![]),
                fds_received: Mutex::new(vec![]),
            }),
            fds_to_close: vec![],
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
        let fds: Vec<RawFd> = {
            let mut v = project.metadata.fds_to_send.lock().unwrap();
            let len = MAX_FDS.min(v.len());
            v.drain(..len).collect()
        };
        
        if fds.len() > 0 {
            match project.inner.send_with_fd(buf, &fds) {
                Ok((sent)) => {
                    //TODO: on linux fds can be closed immediately on OSX, we need to wait until they are accepted by the other party
                    // For now lets leak FDs
                    let mut fds_to_close = fds.into_iter().map(ClosableFd::from).collect();
                    project.fds_to_close.append(&mut fds_to_close);
                    Poll::Ready(Ok(sent))
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    //TODO reinject fds to send
                    project.inner.poll_write_ready(cx).map_ok(|_| 0)
                },
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
                    let mut fds = fds[..descriptors_received].to_vec();
                    project.metadata.fds_received.lock().unwrap().append(&mut fds);

                    buf.assume_init(bytes_received);
                    buf.advance(bytes_received);

                    Poll::Ready(Ok(()))
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    project.inner.poll_read_ready(cx)
                },
                Err(err) => Poll::Ready(Err(err)),
            }
        }
    }

}
