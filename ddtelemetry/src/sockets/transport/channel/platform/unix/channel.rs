use std::{
    io::{self, Write},
    os::unix::{
        net::UnixStream,
        prelude::{FromRawFd, IntoRawFd},
    },
};

use super::PlatformHandle;

#[derive(Debug, Clone)]
pub struct Channel {
    inner: PlatformHandle<UnixStream>,
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
}

impl From<Channel> for PlatformHandle<UnixStream> {
    fn from(c: Channel) -> Self {
        c.inner
    }
}

impl From<PlatformHandle<UnixStream>> for Channel {
    fn from(h: PlatformHandle<UnixStream>) -> Self {
        Channel { inner: h }
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut h = self.inner.as_instance()?;
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
