use std::{
    io::{self, Write, ErrorKind},
    os::unix::{
        net::UnixStream,
        prelude::{FromRawFd, IntoRawFd, AsRawFd, RawFd},
    },
};

use sendfd::SendWithFd;

use super::{PlatformHandle, ChannelMetadata};

#[derive(Debug)]
pub struct Channel {
    inner: PlatformHandle<UnixStream>,
    pub metadata: ChannelMetadata
}

impl Clone for Channel {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), metadata: Default::default() }
    }
}

impl Channel {
    pub fn pair() -> io::Result<ChannelPair> {
        let (local, remote) = UnixStream::pair()?;

        unsafe {
            Ok(ChannelPair(
                PlatformHandle::from_raw_fd(local.into_raw_fd()),
                PlatformHandle::from_raw_fd(remote.into_raw_fd()),
            ))
        }
    }

    pub fn send_message(&mut self, mut buf: &[u8]) -> Result<(), io::Error> {
        let mut socket = self.inner.as_instance()?;

        while !buf.is_empty() {
            let handles = self.metadata.drain_to_send();
            if handles.is_empty() {
                break;
            }

            let fds: Vec<RawFd> = handles.iter().map(AsRawFd::as_raw_fd).collect();
            match socket.send_with_fd(buf, &fds) {
                Ok(0) => {
                    self.metadata.reenqueue_for_sending(handles);
                    return Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                Ok(n) => {
                    self.metadata.defer_close_handles(handles);
                    buf = &buf[n..]
                },
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    self.metadata.reenqueue_for_sending(handles);

                    return Err(e)
                }
            }
        };

        while !buf.is_empty() {
            match socket.write(buf) {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                Ok(n) => buf = &buf[n..],
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl From<Channel> for PlatformHandle<UnixStream> {
    fn from(c: Channel) -> Self {
        c.inner
    }
}

impl From<PlatformHandle<UnixStream>> for Channel {
    fn from(h: PlatformHandle<UnixStream>) -> Self {
        Channel { inner: h, metadata: Default::default() }
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut h = self.inner.as_instance()?;
        // h.send_with_fd(bytes, fds)
        h.write(buf)

    }

    fn flush(&mut self) -> io::Result<()> {
        let mut h = self.inner.as_instance()?;

        h.flush()
    }
}

#[derive(Debug)]
pub struct ChannelPair(PlatformHandle<UnixStream>, PlatformHandle<UnixStream>);

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
            Ok((
                Channel::from(self.0.try_steal()?),
                Channel::from(self.1.try_steal()?),
            ))
        }
    }
}
