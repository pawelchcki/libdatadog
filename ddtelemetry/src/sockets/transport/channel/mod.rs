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

impl super::handles::SerializeHandles for Channel {
    fn serialize_handles<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: super::handles::HandlesSerializer {
        serializer.serialize_handle(&super::handles::Handle::None)
    }
}

