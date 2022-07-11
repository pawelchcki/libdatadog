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

use crate::fork::Forkable;

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewrited, fixed to handle cases like these better.
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

impl TryFrom<Channel> for AsyncChannel {
    type Error = std::io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        Ok(AsyncChannel {
            inner: UnixStream::from_std(value.inner)?,
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
            let res = project.inner.send_with_fd(buf, &fds);
            let mut fds_to_close = fds.into_iter().map(ClosableFd::from).collect();
            
            //TODO: on linux fds can be closed immediately on OSX, we need to wait until they are accepted by the other party
            // For now lets leak FDs
            project.fds_to_close.append(&mut fds_to_close);

            Poll::Ready(res)
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
            let res = project.inner.recv_with_fd(b, &mut fds);

            match res {
                Ok((bytes_received, descriptors_received)) => {
                    let mut fds = fds[..descriptors_received].to_vec();
                    project.metadata.fds_received.lock().unwrap().append(&mut fds);

                    buf.assume_init(bytes_received);
                    buf.advance(bytes_received);

                    Poll::Ready(Ok(()))
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                },
                Err(err) => Poll::Ready(Err(err)),
            }
        }

        // Safety: We trust `TcpStream::read` to have filled up `n` bytes in the
        // buffer.
    }

}
