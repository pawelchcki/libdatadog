use std::{
    ffi::CStr,
    fs::File,
    path::{Path, PathBuf},
};

use ddcommon::cstr;
use spawn_worker::utils::{CListMutPtr, Exact, ExecVec, StartsWith, EndsWith};

// pub(crate) const JAVA_BIN: &[u8] =
//     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../dd-java-agent.jar"));
static JAVA_AGENT_PARAM: &CStr = cstr!("-javaagent:dd-java-agent.jar");

pub unsafe fn check_java_rewrite(cmd: &mut CListMutPtr) -> bool {
    if !Path::new("dd-java-agent.jar").exists() {
        return false;
    }

    return cmd.get_first_entry(EndsWith::from("java")).is_some()
        && cmd.get_entry(Exact::from("-jar")).is_some()
        && cmd.get_entry(StartsWith::from("-javaagent")).is_none();
}

pub unsafe fn rewrite_cmd_args<const N: usize>(cmd: &mut ExecVec<N>) {
    if check_java_rewrite(&mut cmd.as_clist()) {
        cmd.insert_cstr(1, JAVA_AGENT_PARAM);
    }
}
