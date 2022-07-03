use std::{
    os::unix::{
        net::UnixStream as StdUnixStream,
        prelude::{IntoRawFd, RawFd},
    },
    task::Poll,
};

use futures::AsyncRead;
use sendfd::{RecvWithFd, SendWithFd};
use tokio::{io::AsyncWrite, net::UnixStream};

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewrited, fixed to handle cases like these better.
const MAX_FDS: usize = 20;
pub struct Channel {
    inner: StdUnixStream,
}

impl IntoRawFd for Channel {
    fn into_raw_fd(self) -> std::os::unix::prelude::RawFd {
        self.into_raw_fd()
    }
}

impl Channel {
    fn pair() -> std::io::Result<(Self, Self)> {
        let (local, remote) = StdUnixStream::pair()?;

        Ok((Self::from_std(local), Self::from_std(remote)))
    }

    fn from_std(s: StdUnixStream) -> Self {
        Self { inner: s }
    }
}

#[pin_project]
pub struct AsyncChannel {
    #[pin]
    inner: UnixStream,

    fds_to_send: Vec<RawFd>,
    fds_received: Vec<RawFd>,
    fds_to_close: Vec<RawFd>,
}

impl TryFrom<Channel> for AsyncChannel {
    type Error = std::io::Error;

    fn try_from(value: Channel) -> Result<Self, Self::Error> {
        Ok(AsyncChannel {
            inner: UnixStream::from_std(value.inner)?,
            fds_to_send: vec![],
            fds_received: vec![],
            fds_to_close: vec![],
        })
    }
}

impl SendWithFd for AsyncChannel {
    fn send_with_fd(
        &self,
        bytes: &[u8],
        fds: &[std::os::unix::prelude::RawFd],
    ) -> std::io::Result<usize> {
        self.inner.send_with_fd(bytes, fds)
    }
}

impl AsyncWrite for AsyncChannel {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let project = self.project();
        let mut fds: Vec<RawFd> = project.fds_to_send.drain(..MAX_FDS).collect();
        if fds.len() > 0 {
            project.fds_to_close.append(&mut fds);
            //TODO: on linux fds can be closed immediately on OSX, we need to wait until they are accepted by the other party
            // For now lets leak FDs
            Poll::Ready(project.inner.send_with_fd(buf, &fds))
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
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let project = self.project();
        let mut fds = [0; MAX_FDS];
        let res = project.inner.recv_with_fd(buf, &mut fds);

        match res {
            Ok((bytes_received, descriptors_received)) => {
                let mut fds = fds[..descriptors_received].to_vec();
                project.fds_received.append(&mut fds);
                Poll::Ready(Ok(bytes_received))
            }
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}
