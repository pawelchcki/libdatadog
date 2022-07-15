use std::{os::unix::net::UnixStream, sync::Arc};
mod platform;
pub use platform::*;
use serde::{Deserialize, Serialize, Serializer};

impl Serialize for Channel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_none()
    }
}

impl super::handles::HandlesMove for Channel {
    fn move_handles<M>(& self, mover: M) -> Result<M::Ok, M::Error>
    where
        M: super::handles::HandlesTransfer,
    {
        mover.move_handle(&super::handles::Handle::None)
    }
}
