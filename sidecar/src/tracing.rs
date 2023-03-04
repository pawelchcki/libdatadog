use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    ffi::{CStr, CString},
    hash::{Hash, Hasher},
    os::{fd::IntoRawFd, unix::net::UnixStream},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
pub mod trace_events;
pub mod uploader;

use ddtelemetry::ipc::{handles::TransferHandles, platform::PlatformHandle};
use nix::libc;
use rand::Rng;
use serde::{Deserialize, Serialize};
use spawn_worker::utils::{CListMutPtr, EnvKey, ExecVec};

use self::trace_events::{MetaKey, MetaValue, SpanStart};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Id {
    Numeric(u64),
    String(String),
}

impl Default for Id {
    fn default() -> Self {
        // TODO ensure rng is reseeded on fork etc
        Self::Numeric(rand::thread_rng().gen())
    }
}

impl From<Id> for u64 {
    fn from(value: Id) -> Self {
        match value {
            Id::Numeric(id) => id,
            Id::String(s) => {
                let mut hasher = DefaultHasher::new();
                s.hash(&mut hasher);
                hasher.finish()
            }
        }
    }
}

impl ToString for Id {
    fn to_string(&self) -> String {
        match self {
            Id::Numeric(n) => n.to_string(),
            Id::String(s) => s.clone(),
        }
    }
}

impl TryFrom<&str> for Id {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // TODO: add additional validations
        match value.parse() {
            Ok(id) => Ok(Id::Numeric(id)),
            Err(_) => Ok(Id::String(value.to_string())),
        }
    }
}

impl TryFrom<&CStr> for Id {
    type Error = anyhow::Error;

    fn try_from(value: &CStr) -> Result<Self, Self::Error> {
        value.to_string_lossy().trim().try_into()
    }
}
const ENV_TRACE_ID: EnvKey = EnvKey::from("DD_EXEC_T_TRACE_ID");
const ENV_PARENT_SPAN_ID: EnvKey = EnvKey::from("DD_EXEC_T_PARENT_SPAN_ID");
const ENV_SPAN_ID: EnvKey = EnvKey::from("DD_EXEC_T_SPAN_ID");

#[derive(Clone, Debug)]
pub struct TraceContext {
    trace_id: Id,
    span_id: Id,
    parent_span_id: Option<Id>,
}

impl Default for TraceContext {
    fn default() -> Self {
        Self {
            trace_id: Default::default(),
            span_id: Default::default(),
            parent_span_id: None,
        }
    }
}

impl TraceContext {
    pub fn to_child(&self) -> Self {
        let trace_id = self.trace_id.clone();
        let span_id = Id::default();
        let parent_span_id = Some(self.span_id.clone());
        Self {
            trace_id,
            span_id,
            parent_span_id,
        }
    }
    pub fn store_in_c_env<const N: usize>(&self, env: &mut ExecVec<N>) -> anyhow::Result<()> {
        let trace_id = ENV_TRACE_ID.build_c_env(self.trace_id.to_string())?;
        let span_id = ENV_SPAN_ID.build_c_env(self.span_id.to_string())?;
        let parent_span_id = self
            .parent_span_id
            .as_ref()
            .and_then(|p| ENV_PARENT_SPAN_ID.build_c_env(p.to_string()).ok());

        env.push_cstring(trace_id);
        env.push_cstring(span_id);
        if let Some(parent_span_id) = parent_span_id {
            env.push_cstring(parent_span_id);
        }
        Ok(())
    }
    pub unsafe fn extract_from_c_env(env: &mut CListMutPtr) -> Option<Self> {
        let trace_id: Id = env
            .remove_entry(ENV_TRACE_ID)
            .map(EnvKey::get_value)?
            .try_into()
            .ok()?;

        let span_id: Id = env
            .remove_entry(ENV_SPAN_ID)
            .map(EnvKey::get_value)?
            .try_into()
            .ok()?;

        let parent_span_id: Option<Id> = env
            .remove_entry(ENV_PARENT_SPAN_ID)
            .map(EnvKey::get_value)
            .map(TryInto::try_into)
            .and_then(Result::ok);

        Some(Self {
            trace_id,
            span_id,
            parent_span_id,
        })
    }

    pub fn span_start(&self, cmd: &CListMutPtr) -> SpanStart {
        let mut meta = HashMap::new();
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let cmd = unsafe { cmd.to_owned_vec() };
        let expected_len = cmd
            .iter()
            .map(|s| s.to_bytes().len())
            .fold(0, |a, b| a + b + 1);
        let mut cmd_iter = cmd.iter().map(|s| s.to_string_lossy());
        let mut resource = String::with_capacity(expected_len);
        let mut name = None;

        if let Some(s) = cmd_iter.next() {
            let s = s.to_string();
            name = Path::new(&s)
                .file_name()
                .map(|s| s.to_string_lossy().to_string());

            let main_resource_token = name.as_ref().unwrap_or(&s);
            resource.push_str(main_resource_token);
            meta.insert(MetaKey::ExecutablePath, s.into());
        }

        let name = name.unwrap_or_else(|| "unknown".into());

        for s in cmd_iter {
            resource.push(' ');
            if s.contains(' ') {
                resource.push('"');
                resource.push_str(&s);
                resource.push('"');
            } else {
                resource.push_str(&s);
            }
        }

        let service = "executable".into();
        meta.insert(
            MetaKey::CmdOriginPid,
            MetaValue::Pid(nix::unistd::getpid().as_raw()),
        );
        if let Some(s) = nix::unistd::getcwd()
            .ok()
            .and_then(|p| p.to_str().map(ToString::to_string))
        {
            meta.insert(MetaKey::CmdCurrentWorkingDirectory, MetaValue::String(s));
        }

        // create a pair of sockets, to allow remote service to track the lifetime of current process
        let span_end_socket = match UnixStream::pair() {
            Ok((a, b)) => {
                a.into_raw_fd(); // leak socket, it will get automatically closed once the program exits
                Some(PlatformHandle::from(b)) // second socket will get transfered to remote process
            }
            Err(_) => None,
        };

        SpanStart {
            id: self.span_id.clone(),
            trace_id: self.trace_id.clone(),
            parent_id: self.parent_span_id.clone(),
            name,
            resource,
            service,
            time,
            span_end_socket,
            meta,
        }
    }
}
