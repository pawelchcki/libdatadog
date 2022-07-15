use std::{
    error::Error,
    os::unix::{
        net::UnixStream,
        prelude::{IntoRawFd, RawFd},
    },
};

use serde::{Deserialize, Deserializer, Serialize};


#[derive(Serialize,Deserialize, Debug)]
pub enum Handle {
    UnixStream(RawFd),
    Channel(RawFd),
    None,
}

pub trait HandlesTransfer {
    type Ok: Default;

    /// The error type when some error occurs during serialization.
    type Error: Error;

    fn move_handles<'h>(self, handles: Vec<&'h Handle>) -> Result<Self::Ok, Self::Error>;
    fn move_handle<'h>(self, handle: &'h Handle) -> Result<Self::Ok, Self::Error>;
    fn move_none(&self) -> Result<Self::Ok, Self::Error> {
        Ok(Self::Ok::default())
    }
}

pub trait HandlesMove {
    fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer
    ;
}

impl<'h> HandlesMove for Handle {
    fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer
    ,
    {
        mover.move_handle(self)
    }
}

impl<T> HandlesMove for Option<T>
where
    T: HandlesMove,
{
    fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer
    ,
    {
        match self {
            Some(h) => h.move_handles
    (mover),
            None => mover.move_handle(&Handle::None),
        }
    }
}

impl<T> From<Option<T>> for Handle
where
    T: Into<Handle> + Clone,
{
    fn from(h: Option<T>) -> Self {
        match h {
            Some(h) => h.into(),
            None => Handle::None,
        }
    }
}

impl<'h> HandlesMove for (Handle, Handle) {
    fn move_handles<S>(&self, mover: S) -> Result<S::Ok, S::Error>
    where
        S: HandlesTransfer
    ,
    {
        mover.move_handles
(vec![&self.0, &self.1])
    }
}

impl From<UnixStream> for Handle {
    fn from(s: UnixStream) -> Self {
        Handle::UnixStream(s.into_raw_fd())
    }
}

pub struct HandleDeserializer<Deserializer> {
    inner: Deserializer,
}

impl<'de, D > HandleDeserializer<D> where D: Deserializer<'de>{
    pub fn new(inner: D) -> Self {
        Self { inner }
    }
}

impl<T> HandlesMove for tarpc::Response<T> where T: HandlesMove {
    fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer
     {
        if let Ok(message) = &self.message {
            message.move_handles(mover)
        } else {
            Ok(M::Ok::default())
        }
    }
}

impl<T> HandlesMove for tarpc::ClientMessage<T> where T: HandlesMove {
    fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer
     {
        match self {
            tarpc::ClientMessage::Request(r) => r.message.move_handles(mover),
            tarpc::ClientMessage::Cancel { trace_context: _, request_id: _ } => Ok(M::Ok::default()),
            _ => Ok(M::Ok::default()),
        }
    }
}