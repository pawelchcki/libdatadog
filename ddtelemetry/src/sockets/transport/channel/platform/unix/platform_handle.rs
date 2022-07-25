use std::{sync::{atomic::{AtomicI32, Ordering}, Arc}, os::unix::prelude::{RawFd, FromRawFd, AsRawFd, IntoRawFd}, io, marker::PhantomData, mem::MaybeUninit};

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

#[derive(Serialize, Deserialize, Debug)]
pub struct PlatformHandle<T> {
    fd: RawFd, // Just an fd number to be used as reference, not for accessing actual fd
    #[serde(skip)]
    inner: Arc<PlatformHandleInner>,

    phantom: PhantomData<T>
}

impl<T> Clone for PlatformHandle<T> {
    fn clone(&self) -> Self {
        Self { fd: self.fd.clone(), inner: self.inner.clone(), phantom: PhantomData }
    }
}

impl<T> PlatformHandle<T> {
    pub fn try_leak(self) -> Result<RawFd, io::Error> {
        let inner = self.inner.try_unwrap_valid()?;

        Ok(unsafe { inner.leak() })
    }

    pub fn try_unwrap_into(self) -> Result<T, io::Error>
    where
        T: FromRawFd,
    {
        unsafe {
            let fd: RawFd = self.inner.try_unwrap_valid()?.leak();
            Ok(FromRawFd::from_raw_fd(fd))
        }
    }

    pub fn try_clone_valid(&self) -> Result<Self, io::Error> {
        Ok(Self {
            inner: self.inner.try_clone_valid()?,
            fd: self.fd,
            phantom: PhantomData
        })
    }

    pub unsafe fn try_borrow_into(&self) -> Result<T, io::Error>
    where
        T: FromRawFd,
    {
        let fd: RawFd = self.inner.try_access_valid()?.as_raw_fd();
        Ok(FromRawFd::from_raw_fd(fd))
    }

    pub unsafe fn as_borrowed(&self) -> Result<BorrowedHandle<T>, io::Error>  where
    T: FromRawFd + IntoRawFd, {
        let fd: RawFd = self.inner.try_access_valid()?.as_raw_fd();
        let instance = MaybeUninit::new(FromRawFd::from_raw_fd(fd));

        Ok(BorrowedHandle {
            platform_handle: self.try_clone_valid()?,
            inner: instance,
        })
    }

    /// Returns new instance, checking if inner handle is only referenced once and is valid returns error if not
    pub fn try_claim(self) -> Result<Self, io::Error> {
        let inner = Arc::new(self.inner.try_unwrap_valid()?);
        Ok(Self { inner, fd: self.fd, phantom: PhantomData })
    }

    pub unsafe fn try_steal(&self) -> Result<Self, io::Error> {
        let inner = Arc::new(self.inner.try_steal()?);
        Ok(Self {inner, fd: self.fd, phantom: PhantomData})
    }

    pub unsafe fn to_any_type<Y>(self) -> PlatformHandle<Y> {
        PlatformHandle { fd: self.fd, inner: self.inner, phantom: PhantomData }
    }

    pub fn to_rawfd_type(self) -> PlatformHandle<RawFd> {
        unsafe { self.to_any_type() }
    }
}

impl<T> FromRawFd for PlatformHandle<T> {
    /// Creates PlatformHandle instance from supplied RawFd
    ///
    /// # Safety caller must ensure the RawFd is valid and open, and that the resulting PlatformHandle will
    /// # have exclusive ownership of the file descriptor
    ///
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        let inner = Arc::new(PlatformHandleInner::from_raw_fd(fd));
        Self { fd, inner, phantom: PhantomData }
    }
}

impl<T> AsRawFd for PlatformHandle<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

#[derive(Debug)]
struct PlatformHandleInner {
    fd: AtomicI32,
}

impl PlatformHandleInner {
    #[inline] // TODO consider removing this
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

impl<T> Default for PlatformHandle<T> {
    fn default() -> Self {
        let inner = Arc::from(PlatformHandleInner::default());
        let fd = inner.as_raw_fd();
        Self { fd, inner, phantom: PhantomData }
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
pub struct BorrowedHandle<T>
where
    T: IntoRawFd,
{
    platform_handle: PlatformHandle<T>,
    inner: MaybeUninit<T>,
}

impl<T> core::ops::Deref for BorrowedHandle<T>
where
    T: IntoRawFd,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.inner.assume_init_ref() }
    }
}

impl<T> std::ops::DerefMut for BorrowedHandle<T>
where
    T: IntoRawFd,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.inner.assume_init_mut() }
    }
}

impl<T> Drop for BorrowedHandle<T>
where
    T: IntoRawFd,
{
    fn drop(&mut self) {
        let uninit = MaybeUninit::uninit();
        let inner = std::mem::replace(&mut self.inner, uninit);
        let inner = unsafe { inner.assume_init() };
        inner.into_raw_fd(); // leak handle knowing it was just a reference
    }
}