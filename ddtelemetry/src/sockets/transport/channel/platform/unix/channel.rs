use std::{
    io::{self, Write, ErrorKind, Read},
    os::unix::{
        net::UnixStream,
        prelude::{FromRawFd, IntoRawFd, AsRawFd, RawFd},
    }, time::Duration,
};

use sendfd::{SendWithFd, RecvWithFd};

use super::{PlatformHandle, ChannelMetadata, MAX_FDS};

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

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<(), io::Error> {
        let sock = self.inner.as_instance()?;
        sock.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> Result<(), io::Error> {
        let sock = self.inner.as_instance()?;
        sock.set_write_timeout(timeout)
    }
}

impl Read for Channel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        println!("reading");
        let mut fds = [0; MAX_FDS];
        let socket = self.inner.as_instance()?;
        let (n, fd_cnt) = socket.recv_with_fd(buf, &mut fds)?;
        println!("read: {}", n);
        self.metadata.receive_fds(&fds[..fd_cnt]);
        Ok(n)
    }
}

impl Write for Channel {
    fn write_all(&mut self, mut buf: &[u8]) -> Result<(), io::Error> {
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

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        //TODO implement partial writes
        self.write_all(buf).map(|_| buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut socket = self.inner.as_instance()?;
        socket.flush()
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
