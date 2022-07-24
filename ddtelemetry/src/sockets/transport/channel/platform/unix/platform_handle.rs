use std::{sync::{atomic::{AtomicI32, Ordering}, Arc}, os::unix::prelude::{RawFd, FromRawFd, AsRawFd}, io};

use serde::{Serialize, Deserialize};


pub type NativeRawHandle = RawFd;

pub trait FromNativeRawHandle: FromRawFd {
    unsafe fn from_raw_native_handle(handle: NativeRawHandle) -> Self;
}

impl<T> FromNativeRawHandle for T
where
    T: FromRawFd + Sized,
{
    unsafe fn from_raw_native_handle(handle: NativeRawHandle) -> Self {
        FromRawFd::from_raw_fd(handle)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlatformHandle {
    fd: RawFd, // Just an fd number to be used as reference, not for accessing actual fd
    #[serde(skip)]
    inner: Arc<PlatformHandleInner>,
}

///
impl PlatformHandle {
    pub fn try_leak(self) -> Result<RawFd, io::Error> {
        let inner = self.inner.try_unwrap_valid()?;

        Ok(unsafe { inner.leak() })
    }

    pub unsafe fn try_unwrap_into<T>(self) -> Result<T, io::Error>
    where
        T: FromRawFd,
    {
        let fd: RawFd = self.inner.try_unwrap_valid()?.leak();
        Ok(FromRawFd::from_raw_fd(fd))
    }

    pub fn try_clone_valid(&self) -> Result<Self, io::Error> {
        Ok(Self {
            inner: self.inner.clone(),
            fd: self.inner.as_raw_fd(),
        })
    }

    pub unsafe fn try_borrow_into<T>(&self) -> Result<T, io::Error>
    where
        T: FromRawFd,
    {
        let fd: RawFd = self.inner.try_access_valid()?.as_raw_fd();
        Ok(FromRawFd::from_raw_fd(fd))
    }

    /// Returns new instance, checking if inner handle is only referenced once and is valid returns error if not
    pub fn try_claim(self) -> Result<Self, io::Error> {
        let inner = Arc::new(self.inner.try_unwrap_valid()?);
        let fd = inner.as_raw_fd();
        Ok(Self { inner, fd })
    }

    pub unsafe fn try_steal(&self) -> Result<Self, io::Error> {
        let inner = Arc::new(self.inner.try_steal()?);
        let fd = inner.as_raw_fd();
        Ok(Self {inner, fd})
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UnsafePlatformHandle(PlatformHandle);

impl UnsafePlatformHandle {
    pub unsafe fn leak(&self) -> RawFd {
        unsafe { self.0.inner.leak() }
    }

    pub unsafe fn try_claim_into<T>(self) -> Result<T, io::Error>
    where
        T: FromRawFd,
    {
        let fd: RawFd = self.0.inner.try_claim()?.as_raw_fd();
        Ok(FromRawFd::from_raw_fd(fd))
    }

    /// Creates new instance with transfered ownership of a valid handle
    pub unsafe fn try_claim(&self) -> Result<PlatformHandle, io::Error> {
        let inner = Arc::new(self.0.inner.try_claim()?);
        Ok(PlatformHandle {
            inner,
            fd: self.0.fd,
        })
    }

    pub unsafe fn unchecked_into(self) -> PlatformHandle {
        PlatformHandle {
            inner: self.0.inner,
            fd: self.0.fd,
        }
    }
}

impl FromRawFd for PlatformHandle {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    ///
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        let inner = Arc::new(PlatformHandleInner::from_raw_fd(fd));
        Self { fd, inner }
    }
}

impl PlatformHandle {}

impl FromRawFd for UnsafePlatformHandle {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Self(PlatformHandle::from_raw_fd(fd))
    }
}

impl AsRawFd for PlatformHandle {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

#[derive(Debug)]
struct PlatformHandleInner {
    fd: AtomicI32,
}

impl PlatformHandleInner {
    #[inline]
    pub unsafe fn leak(&self) -> RawFd {
        // prevent FD from being closed on drop
        self.fd.swap(-1, Ordering::AcqRel)
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.as_raw_fd() >= 0
    }

    /// Transfers ownership of the FD into a new instance
    /// Old instance effectively no longer will be able to use the FD
    ///
    /// Returns error if FD instance was unowned or invalid
    pub unsafe fn try_claim(&self) -> Result<Self, io::Error> {
        let claim = Self::from_raw_fd(self.leak());
        claim.try_access_valid()?;
        Ok(claim)
    }

    /// Transfers ownership of the FD into a new instance
    /// Old instance effectively no longer will be able to use the FD
    ///
    /// Returns error if FD instance was unowned or invalid
    pub unsafe fn try_steal(&self) -> Result<Self, io::Error> {
        let claim = Self::from_raw_fd(self.leak());
        claim.try_access_valid()?;
        Ok(claim)
    }

    fn try_clone_valid(self: &Arc<Self>) -> Result<Arc<Self>, io::Error> {
        let handle = Arc::clone(self);
        handle.try_access_valid()?;
        Ok(handle)
    }

    pub fn try_unwrap_valid(self: Arc<Self>) -> Result<Self, io::Error> {
        let handle = Arc::try_unwrap(self).map_err(|inner| {
            io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "attempting to unwrap FD from shared platform handle (fd: {})",
                    inner.as_raw_fd()
                ),
            )
        })?;
        handle.try_access_valid()?;
        Ok(handle)
    }

    pub fn try_access_valid(&self) -> Result<&Self, io::Error> {
        if !self.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "attempting to accesss FD from unowned platform handle (fd: {})",
                    self.as_raw_fd()
                ),
            ));
        }
        Ok(self)
    }
}

impl AsRawFd for PlatformHandleInner {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.fd.load(Ordering::Acquire)
    }
}

impl FromRawFd for PlatformHandleInner {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Self {
            fd: AtomicI32::new(fd),
        }
    }
}

impl Default for PlatformHandleInner {
    fn default() -> Self {
        Self {
            fd: AtomicI32::new(-1),
        }
    }
}

impl Default for PlatformHandle {
    fn default() -> Self {
        let inner = Arc::from(PlatformHandleInner::default());
        let fd = inner.as_raw_fd();
        Self { fd, inner }
    }
}

impl Drop for PlatformHandleInner {
    fn drop(&mut self) {
        let fd = unsafe { self.leak() };

        if fd >= 0 {
            unsafe {
                let _ = libc::close(fd);
            }
        }
    }
}
