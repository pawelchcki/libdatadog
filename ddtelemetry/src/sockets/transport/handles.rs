use std::{
    error::Error,
    fs::File,
    marker::PhantomData,
    os::unix::{
        net::UnixStream,
        prelude::{IntoRawFd, RawFd},
    }, sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize};

use super::channel::PlatformHandle;

#[derive(Serialize, Deserialize, Debug)]
pub enum Handle {
    UnixStream(RawFd),
    Channel(RawFd),
    File(RawFd),
    None,
}

pub trait HandlesTransfer {
    type Ok: Default;

    /// The error type when some error occurs during serialization.
    type Error: Error;

    fn move_handles<'h>(self, handles: Vec<&'h Handle>) -> Result<Self::Ok, Self::Error>;
    fn move_handle<'h>(self, handle: &'h Handle) -> Result<Self::Ok, Self::Error>;
    fn move_bhandle<'h, T>(self, handle: BetterHandle<T>) -> Result<Self::Ok, Self::Error>;
    fn move_none(&self) -> Result<Self::Ok, Self::Error> {
        Ok(Self::Ok::default())
    }
}

pub trait HandlesProvider {
    type Error: Error;

    fn provide_handle(&self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error>;
}

pub trait HandlesReceive {
    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesProvider;
}

pub trait HandlesMove {
    fn move_handles<M>(&self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer;
}

impl<'h> HandlesMove for Handle {
    fn move_handles<M>(& self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer,
    {
        mover.move_handle(self)
    }
}

impl<T> HandlesMove for Option<T>
where
    T: HandlesMove,
{
    fn move_handles<M>(& self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer,
    {
        match self {
            Some(h) => h.move_handles(mover),
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
    fn move_handles<S>(& self, mover: S) -> Result<S::Ok, S::Error>
    where
        S: HandlesTransfer,
    {
        mover.move_handles(vec![&self.0, &self.1])
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

impl<'de, D> HandleDeserializer<D>
where
    D: Deserializer<'de>,
{
    pub fn new(inner: D) -> Self {
        Self { inner }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BetterHandle<T> {
    inner: PlatformHandle,
    phantom: PhantomData<T>,
}

impl<T> Clone for BetterHandle<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), phantom: self.phantom.clone() }
    }
}

impl<T> From<PlatformHandle> for BetterHandle<T> {
    fn from(h: PlatformHandle) -> Self {
        Self {
            inner: h,
            phantom: PhantomData,
        }
    }
}

impl<T> From<BetterHandle<T>> for PlatformHandle {
    fn from(h: BetterHandle<T>) -> Self {
        h.inner
    }
}

impl<T> Default for BetterHandle<T> {
    fn default() -> Self {
        Self { inner: Default::default(), phantom: PhantomData }
    }
} 

impl<T> BetterHandle<T> {
    pub fn as_platform_handle<'a>(&'a self) -> &'a PlatformHandle {
        &self.inner
    }
}

impl From<File> for BetterHandle<File> {
    fn from(f: File) -> Self {
        BetterHandle {
            inner: f.into(),
            phantom: PhantomData,
        }
    }
}

impl TryFrom<BetterHandle<File>> for File {
    type Error = <File as TryFrom<PlatformHandle>>::Error;

    fn try_from(value: BetterHandle<File>) -> Result<Self, Self::Error> {
        value.inner.try_into()
    }
}

impl<T> HandlesMove for BetterHandle<T> {
    fn move_handles<M>(& self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: HandlesTransfer,
    {
        mover.move_bhandle(self.clone())
    }
}

impl<T> HandlesReceive for BetterHandle<T> {
    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesProvider,
    {
        let p = provider.provide_handle(self.as_platform_handle())?;
        self.inner = p;
        Ok(())
    }
}

mod tarpc_impl {
    use super::{HandlesProvider, HandlesReceive, HandlesMove, HandlesTransfer};


    impl<T> HandlesMove for tarpc::Response<T>
    where
        T: HandlesMove,
    {
        fn move_handles<M>(& self, mover: M) -> Result<M::Ok, M::Error>
        where
            M: HandlesTransfer,
        {
            if let Ok(message) = &self.message {
                message.move_handles(mover)
            } else {
                Ok(M::Ok::default())
            }
        }
    }
    
    impl<T> HandlesMove for tarpc::ClientMessage<T>
    where
        T: HandlesMove,
    {
        fn move_handles<M>(& self, mover: M) -> Result<M::Ok, M::Error>
        where
            M: HandlesTransfer,
        {
            match self {
                tarpc::ClientMessage::Request(r) => r.message.move_handles(mover),
                tarpc::ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(M::Ok::default()),
                _ => Ok(M::Ok::default()),
            }
        }
    }


    impl<T> HandlesReceive for tarpc::Response<T>
    where
        T: HandlesReceive,
    {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesProvider,
        {
            if let Ok(message) = &mut self.message {
                message.receive_handles(provider)?;
            }
            Ok(())
        }
    }

    impl<T> HandlesReceive for tarpc::ClientMessage<T>
    where
        T: HandlesReceive,
    {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesProvider,
        {
            match self {
                tarpc::ClientMessage::Request(r) => r.receive_handles(provider),
                tarpc::ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(()),
                _ => Ok(()),
            }
        }
    }

    impl<T> HandlesReceive for tarpc::Request<T>
    where
        T: HandlesReceive,
    {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesProvider,
        {
            self.message.receive_handles(provider)
        }
    }
}

pub use tarpc_impl::*;