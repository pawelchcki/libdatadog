use std::{error::Error, fs::File, marker::PhantomData};

use serde::{Deserialize, Serialize};

use super::channel::PlatformHandle;

pub trait HandlesTransport {
    /// The error type when some error occurs during serialization.
    type Error: Error;

    fn move_handle<T>(self, handle: BetterHandle<T>) -> Result<(), Self::Error>;
    fn provide_handle(self, hint: &PlatformHandle) -> Result<PlatformHandle, Self::Error>;
}

pub trait TransferHandles {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error>;

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error>;
}

mod transport_impls {
    use super::{HandlesTransport, TransferHandles};

    impl<T, E> TransferHandles for Result<T, E>
    where
        T: TransferHandles,
    {
        fn move_handles<Transport>(&self, transport: Transport) -> Result<(), Transport::Error>
        where
            Transport: HandlesTransport,
        {
            match self {
                Ok(i) => i.move_handles(transport),
                Err(_) => Ok(()),
            }
        }

        fn receive_handles<Transport>(
            &mut self,
            transport: Transport,
        ) -> Result<(), Transport::Error>
        where
            Transport: HandlesTransport,
        {
            match self {
                Ok(i) => i.receive_handles(transport),
                Err(_) => Ok(()),
            }
        }
    }

    use tarpc::{ClientMessage, Request, Response};

    impl<T: TransferHandles> TransferHandles for Response<T> {
        fn move_handles<Transport: HandlesTransport>(
            &self,
            transport: Transport,
        ) -> Result<(), Transport::Error> {
            self.message.move_handles(transport)
        }

        fn receive_handles<Transport: HandlesTransport>(
            &mut self,
            transport: Transport,
        ) -> Result<(), Transport::Error> {
            self.message.receive_handles(transport)
        }
    }

    impl<T> TransferHandles for ClientMessage<T>
    where
        T: TransferHandles,
    {
        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            match self {
                ClientMessage::Request(r) => r.move_handles(mover),
                ClientMessage::Cancel {
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
                ClientMessage::Request(r) => r.receive_handles(provider),
                ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(()),
                _ => Ok(()),
            }
        }
    }

    impl<T: TransferHandles> TransferHandles for Request<T> {
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
    }
}

pub use transport_impls::*;

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
