use std::{
    error::Error,
    fs::File,
    marker::PhantomData,
};

use serde::{Deserialize, Serialize};

use super::channel::PlatformHandle;

pub trait HandlesTransfer {
    /// The error type when some error occurs during serialization.
    type Error: Error;

    fn move_handle<'h, T>(self, handle: BetterHandle<T>) -> Result<(), Self::Error>;
}

pub trait HandlesProvider {
    type Error: Error;

    fn provide_handle(self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error>;
}

pub trait HandlesReceive {
    fn receive_handles<P>(&mut self, _provider: P) -> Result<(), P::Error>
    where
        P: HandlesProvider {
            Ok(())
        }
}

pub trait HandlesMove {
    fn move_handles<M>(&self, _: M) -> Result<(), M::Error>
    where
        M: HandlesTransfer {
            Ok(())
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

impl<T> From<T> for BetterHandle<T> where T: Into<PlatformHandle>{
    fn from(h: T) -> Self {
        Self {
            inner: h.into(),
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

impl TryFrom<BetterHandle<File>> for File {
    type Error = <File as TryFrom<PlatformHandle>>::Error;

    fn try_from(value: BetterHandle<File>) -> Result<Self, Self::Error> {
        value.inner.try_into()
    }
}

impl<T> HandlesMove for BetterHandle<T> {
    fn move_handles<M>(& self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransfer,
    {
        mover.move_handle(self.clone())
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
        fn move_handles<M>(& self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransfer,
        {
            if let Ok(message) = &self.message {
                message.move_handles(mover)
            } else {
                Ok(())
            }
        }
    }
    
    impl<T> HandlesMove for tarpc::ClientMessage<T>
    where
        T: HandlesMove,
    {
        fn move_handles<M>(& self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransfer,
        {
            match self {
                tarpc::ClientMessage::Request(r) => r.message.move_handles(mover),
                tarpc::ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(()),
                _ => Ok(()),
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