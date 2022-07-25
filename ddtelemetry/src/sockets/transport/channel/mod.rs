mod platform;
pub use platform::*;
use tokio_serde::formats::MessagePack;

pub type DefaultCodec<Item, SinkItem> = MessagePack<Item, SinkItem>;
