use std::{
    collections::{BTreeMap, VecDeque},
    io,
    os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
};

use serde::{Deserialize, Serialize};

use crate::{
    fork::{getpid, ForkSafe},
    sockets::transport::handles::{HandlesTransport, TransferHandles},
};

mod platform_handle;
pub use platform_handle::*;

mod channel;
pub use channel::*;

mod async_channel;
pub use async_channel::*;

/// sendfd crate's API is not able to resize the received FD container.
/// limiting the max number of sent FDs should allow help lower a chance of surprise
/// TODO: sendfd should be rewriten, fixed to handle cases like these better.
pub const MAX_FDS: usize = 20;

#[derive(Deserialize, Serialize)]
pub struct Message<Item> {
    pub item: Item,
    pub acked_handles: Vec<RawFd>,
    pub pid: libc::pid_t,
}

impl<Item> Message<Item> {
    pub fn ref_item<'a>(&'a self) -> &'a Item {
        &self.item
    }
}

impl<T> TransferHandles for Message<T>
where
    T: TransferHandles,
{
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransport,
    {
        self.item.move_handles(mover)
    }

    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesTransport,
    {
        self.item.receive_handles(provider)
    }
}

impl<T> From<T> for PlatformHandle<T>
where
    T: IntoRawFd,
{
    fn from(h: T) -> Self {
        unsafe { PlatformHandle::from_raw_fd(h.into_raw_fd()) }
    }
}

impl ForkSafe for &ChannelPair {}

#[derive(Debug, Clone)]
pub struct ChannelMetadata {
    fds_to_send: Vec<PlatformHandle<RawFd>>,
    fds_received: VecDeque<RawFd>,
    fds_acked: Vec<RawFd>,
    fds_to_close: BTreeMap<RawFd, PlatformHandle<RawFd>>,
    pid: libc::pid_t, // must always be set to current Process ID
}

impl Default for ChannelMetadata {
    fn default() -> Self {
        Self {
            fds_to_send: Default::default(),
            fds_received: Default::default(),
            fds_acked: Default::default(),
            fds_to_close: Default::default(),
            pid: getpid(),
        }
    }
}

impl HandlesTransport for &mut ChannelMetadata {
    type Error = io::Error;

    fn move_handle<'h, T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        self.enqueue_for_sending(handle);

        Ok(())
    }

    fn provide_handle<T>(self, hint: &PlatformHandle<T>) -> Result<PlatformHandle<T>, Self::Error> {
        self.find_handle(hint).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "can't provide expected handle for hint: {}",
                    hint.as_raw_fd()
                ),
            )
        })
    }
}

impl ChannelMetadata {
    pub fn unwrap_message<T>(&mut self, message: Message<T>) -> Result<T, io::Error>
    where
        T: TransferHandles,
    {
        {
            // close all open file desriptors that were ACKed by the other party
            let fds_to_close: Vec<PlatformHandle<RawFd>> = message
                .acked_handles
                .into_iter()
                .flat_map(|fd| self.fds_to_close.remove(&fd))
                .collect();

            // if ACK came from the same PID, it means there is a duplicate PlatformHandle instance in the same
            // process. Thus we should leak the handles allowing other PlatformHandle's to safely close
            if message.pid == self.pid {
                for h in fds_to_close.into_iter() {
                    h.try_leak().unwrap_or_default();
                }
            }
        }
        let mut item = message.item;

        item.receive_handles(self)?;
        Ok(item)
    }

    pub fn create_message<T>(&mut self, item: T) -> Result<Message<T>, io::Error>
    where
        T: TransferHandles,
    {
        item.move_handles(&mut *self)?;

        let message = Message {
            item,
            acked_handles: self.fds_acked.drain(..).collect(),
            pid: self.pid,
        };

        Ok(message)
    }

    pub(crate) fn defer_close_handles<T>(&mut self, handles: Vec<PlatformHandle<T>>) {
        let handles = handles
            .into_iter()
            .map(|h| (h.as_raw_fd(), h.to_rawfd_type()));
        self.fds_to_close.extend(handles);
    }

    pub(crate) fn enqueue_for_sending<T>(&mut self, handle: PlatformHandle<T>) {
        self.fds_to_send.push(handle.to_rawfd_type())
    }

    pub(crate) fn reenqueue_for_sending(&mut self, mut handles: Vec<PlatformHandle<RawFd>>) {
        handles.append(&mut self.fds_to_send);
        self.fds_to_send = handles;
    }

    pub(crate) fn drain_to_send(&mut self) -> Vec<PlatformHandle<RawFd>> {
        let drain = self
            .fds_to_send
            .drain(..)
            .filter_map(|h| h.try_claim().ok());

        let mut cnt: i32 = MAX_FDS.try_into().unwrap_or(i32::MAX);

        let (to_send, leftover) = drain.partition(|_| {
            cnt -= 1;
            cnt >= 0
        });
        self.reenqueue_for_sending(leftover);

        to_send
    }

    pub(crate) fn find_handle<T>(&mut self, hint: &PlatformHandle<T>) -> Option<PlatformHandle<T>> {
        if hint.as_raw_fd() < 0 {
            return Some(hint.clone());
        }

        let fd = self.fds_received.pop_front();

        fd.map(|fd| unsafe { PlatformHandle::from_raw_fd(fd) })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::{
        collections::BTreeMap,
        fs::File,
        io::{self, Read, Seek, Write},
        os::unix::prelude::{AsRawFd, RawFd},
        path::Path,
    };

    use crate::sockets::transport::channel::MAX_FDS;

    use super::{ChannelMetadata, PlatformHandle};

    fn leaked_handle() -> PlatformHandle<RawFd> {
        let file = tempfile::tempfile().unwrap();
        let handle = PlatformHandle::from(file);
        unsafe {
            handle.try_steal().unwrap();
        }

        handle.to_rawfd_type()
    }

    fn assert_platform_handle_is_valid_file(
        handle: PlatformHandle<RawFd>,
    ) -> PlatformHandle<RawFd> {
        let handle = handle.try_claim().unwrap();
        let mut file: File = unsafe { handle.to_any_type().into_instance().unwrap() };

        write!(file, "test_string").unwrap();
        file.rewind().unwrap();

        let mut data = String::new();
        file.read_to_string(&mut data).unwrap();
        assert_eq!("test_string", data);

        file.rewind().unwrap();
        PlatformHandle::from(file).to_rawfd_type()
    }

    fn get_open_file_descriptors(
        pid: Option<libc::pid_t>,
    ) -> Result<BTreeMap<RawFd, String>, io::Error> {
        let proc = pid.map(|p| format!("{}", p)).unwrap_or("self".into());

        let fds_path = Path::new("/proc").join(proc).join("fd");
        let fds = std::fs::read_dir(fds_path)?
            .filter_map(|r| r.ok())
            .filter_map(|r| {
                let link = std::fs::read_link(r.path()).unwrap_or_default();
                let link = link.into_os_string().into_string().ok().unwrap_or_default();
                let fd = r.file_name().into_string().ok().unwrap_or_default();
                fd.parse().ok().map(|fd| (fd, link))
            })
            .collect();

        Ok(fds)
    }

    fn assert_file_descriptors_unchanged(
        reference_meta: &BTreeMap<RawFd, String>,
        pid: Option<libc::pid_t>,
    ) {
        let current_meta = get_open_file_descriptors(pid).unwrap();

        // let missing_from_reference = current_fds.into()
        assert_eq!(reference_meta, &current_meta);
    }

    #[test]
    fn test_channel_metadata_only_provides_valid_owned() {
        let reference = get_open_file_descriptors(None).unwrap();
        let mut meta = ChannelMetadata::default();

        // enqueue invalid platform handles those should be dropped before sending
        for _h in 0..10 {
            meta.enqueue_for_sending(leaked_handle())
        }

        // create real handles
        let files: Vec<File> = (0..)
            .map(|_| tempfile::tempfile().unwrap())
            .take(MAX_FDS * 2)
            .collect();
        let reference_open_files = get_open_file_descriptors(None).unwrap();

        // used for checking order of reenqueue behaviour
        let file_fds: Vec<RawFd> = files.iter().map(AsRawFd::as_raw_fd).collect();

        files
            .into_iter()
            .for_each(|f| meta.enqueue_for_sending(f.into()));

        let first_batch: Vec<PlatformHandle<RawFd>> = meta
            .drain_to_send()
            .into_iter()
            .map(assert_platform_handle_is_valid_file)
            .collect();

        assert_eq!(MAX_FDS, first_batch.len());

        meta.reenqueue_for_sending(first_batch);

        let mut handles = meta.drain_to_send();
        let second_batch = meta.drain_to_send();

        handles.extend(second_batch.into_iter());
        assert_eq!(MAX_FDS * 2, handles.len());
        assert_eq!(0, meta.drain_to_send().len());

        let final_ordered_fds_list: Vec<RawFd> = handles.iter().map(AsRawFd::as_raw_fd).collect();
        assert_eq!(file_fds, final_ordered_fds_list);

        assert_file_descriptors_unchanged(&reference_open_files, None);

        // test and dispose of all handles
        for handle in handles {
            assert_platform_handle_is_valid_file(handle);
        }

        assert_file_descriptors_unchanged(&reference, None);
    }
}
