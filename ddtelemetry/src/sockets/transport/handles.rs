use std::{error::Error, sync::Arc, os::unix::{net::UnixStream, prelude::{RawFd, IntoRawFd}}};

use serde::{Serialize, Serializer};

use super::channel::Channel;

pub enum Handle{
    UnixStream(UnixStream),
    Channel(Channel),
    None
}

impl IntoRawFd for Handle {
    fn into_raw_fd(self) -> RawFd {
        match self {
            Handle::UnixStream(s) => s.into_raw_fd(),
            Handle::Channel(c) => c.into_raw_fd(),
            Handle::None => -1,
        }
    }
}

pub trait HandlesSerializer {
    type Ok;

    /// The error type when some error occurs during serialization.
    type Error: Error;

    fn serialize_handles<'h>(self, handles: Vec<&'h Handle>) -> Result<Self::Ok, Self::Error>;
    fn serialize_handle<'h>(self, handle: &'h Handle) -> Result<Self::Ok, Self::Error>;
}

pub trait SerializeHandles {
    fn serialize_handles<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: HandlesSerializer;
}

impl<'h> SerializeHandles for Handle {
    fn serialize_handles<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: HandlesSerializer {
        serializer.serialize_handle(self)
    }
}

impl<T> SerializeHandles for Option<T> where T: SerializeHandles {
    fn serialize_handles<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: HandlesSerializer {
        match self {
            Some(h) => h.serialize_handles(serializer),
            None => serializer.serialize_handle(&Handle::None),
        }
    }
}

impl<T> From<Option<T>> for Handle where T: Into<Handle> + Clone {
    fn from(h: Option<T>) -> Self {
        match h {
            Some(h) => h.into(),
            None => Handle::None,
        }
    }
}

impl<'h> SerializeHandles for (Handle, Handle) {
    fn serialize_handles<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: HandlesSerializer {
        serializer.serialize_handles(vec![&self.0, &self.1])
    }
}

impl From<UnixStream> for Handle {
    fn from(s: UnixStream) -> Self {
        Handle::UnixStream(s)
    }
}

impl Serialize for Handle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
        serializer.serialize_none()
    }
}