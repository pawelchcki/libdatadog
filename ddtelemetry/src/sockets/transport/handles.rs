use std::{error::Error, fs::File, marker::PhantomData};

use serde::{Deserialize, Serialize};

use super::channel::PlatformHandle;

pub trait HandlesTransport {
    /// The error type when some error occurs during serialization.
    type Error: Error;

    fn move_handle<'h, T>(self, handle: BetterHandle<T>) -> Result<(), Self::Error>;
    fn provide_handle(self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error>;
}

pub trait TransferHandles {
    fn move_handles<M>(&self, _: M) -> Result<(), M::Error>
    where
        M: HandlesTransport,
    {
        Ok(())
    }

    fn receive_handles<P>(&mut self, _provider: P) -> Result<(), P::Error>
    where
        P: HandlesTransport,
    {
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
        Self {
            inner: self.inner.clone(),
            phantom: self.phantom.clone(),
        }
    }
}

impl<T> From<T> for BetterHandle<T>
where
    T: Into<PlatformHandle>,
{
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
        Self {
            inner: Default::default(),
            phantom: PhantomData,
        }
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

impl<T> TransferHandles for BetterHandle<T> {
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransport,
    {
        mover.move_handle(self.clone())
    }

    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesTransport,
    {
        let p = provider.provide_handle(self.as_platform_handle())?;
        self.inner = p;
        Ok(())
    }
}

mod tarpc_impl {
    use super::{HandlesTransport, TransferHandles};

    impl<T> TransferHandles for tarpc::Response<T>
    where
        T: TransferHandles,
    {
        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            if let Ok(message) = &self.message {
                message.move_handles(mover)
            } else {
                Ok(())
            }
        }

  

        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesTransport,
        {
            if let Ok(message) = &mut self.message {
                message.receive_handles(provider)?;
            }
            Ok(())
        }
    }

    impl<T> TransferHandles for tarpc::ClientMessage<T>
    where
        T: TransferHandles,
    {
        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            match self {
                tarpc::ClientMessage::Request(r) => r.move_handles(mover),
                tarpc::ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(()),
                _ => Ok(()),
            }
        }
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesTransport,
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



    impl<T> TransferHandles for tarpc::Request<T>
    where
        T: TransferHandles,
    {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesTransport,
        {
            self.message.receive_handles(provider)
        }

        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransport,
    {
                self.message.move_handles(mover)

    }

        // fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        // where
        //     M: HandlesTransport,
        // {
        //     self.message.move_handle(mover)
        // }
    }
}

pub use tarpc_impl::*;
