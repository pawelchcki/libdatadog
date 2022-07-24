use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, Write},
    os::unix::prelude::{AsRawFd, FromRawFd, RawFd},
    path::Path,
};
use pretty_assertions::{assert_eq, assert_ne};

use crate::sockets::transport::channel::MAX_FDS;

use super::{ChannelMetadata, PlatformHandle};

fn mock_handle(mock_fd: RawFd) -> PlatformHandle {
    unsafe {
        let handle = PlatformHandle::from_raw_fd(mock_fd);
        handle.leak(); // avoid platform handle calling close on non existing fd
        handle
    }
}

fn assert_platform_handle_is_valid_file(handle: PlatformHandle) -> PlatformHandle {
    let handle = handle.claim_valid().unwrap();
    let mut file = unsafe { File::from_raw_fd(handle.leak()) };

    // Should return none, as handle's FD was leaked
    assert_eq!(true, handle.claim_valid().is_none());
    write!(file, "test_string").unwrap();
    file.rewind().unwrap();

    let mut data = String::new();
    file.read_to_string(&mut data).unwrap();
    assert_eq!("test_string", data);

    file.rewind().unwrap();
    file.into()
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
    (-1..10)
        .map(mock_handle)
        .for_each(|h| meta.enqueue_for_sending(h));

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

    let first_batch: Vec<PlatformHandle> = meta
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

    // let mut expected_open_file_descriptors = reference.clone();
    // expected_open_file_descriptors.extend(file_fds.into_iter().map(|f| (f, String::new())));

    assert_file_descriptors_unchanged(&reference_open_files, None);

    // test and dispose of all handles
    for handle in handles {
        assert_platform_handle_is_valid_file(handle);
    }

    assert_file_descriptors_unchanged(&reference_open_files, None);
    assert_file_descriptors_unchanged(&reference, None);
}
