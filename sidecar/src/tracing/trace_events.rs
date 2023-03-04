use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
    os::unix::net::UnixStream,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use datadog_trace_protobuf::pb::{self, TracerPayload};
use ddtelemetry::{
    ipc::{handles::TransferHandles, platform::PlatformHandle},
    uuid,
};
use nix::libc;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};

use super::Id;
fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum MetaKey {
    String(String),
    CmdCurrentWorkingDirectory,
    CmdOriginPid,
    ExecutablePath,
}

impl ToString for MetaKey {
    fn to_string(&self) -> String {
        match self {
            MetaKey::String(s) => s.clone(),
            MetaKey::CmdCurrentWorkingDirectory => "current_working_directory".into(),
            MetaKey::CmdOriginPid => "pid".into(),
            MetaKey::ExecutablePath => "executable_path".into(),  
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum MetaValue {
    String(String),
    Pid(libc::pid_t),
}

impl From<String> for MetaValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl ToString for MetaValue {
    fn to_string(&self) -> String {
        match self {
            MetaValue::String(s) => s.clone(),
            MetaValue::Pid(p) => p.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum Event {
    StartSpan(Box<SpanStart>),
    SpanFinished(SpanFinished),
    ReportError(ReportError), //todo unused yet
}

impl Event {
    pub fn trace_id(&self) -> &Id {
        match self {
            Event::StartSpan(s) => &s.trace_id,
            Event::SpanFinished(s) => &s.trace_id,
            Event::ReportError(e) => &e.trace_id,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportError {
    trace_id: Id,
    span_id: Id,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpanStart {
    pub id: Id,
    pub name: String,
    pub trace_id: Id,
    pub parent_id: Option<Id>,
    pub resource: String,
    pub service: String,
    pub time: u64,
    pub span_end_socket: Option<PlatformHandle<UnixStream>>,
    pub meta: HashMap<MetaKey, MetaValue>,
}

impl From<Box<SpanStart>> for pb::Span {
    fn from(span_start: Box<SpanStart>) -> Self {
        let mut meta = HashMap::new();
        for (k,v) in span_start.meta {
            meta.insert(k.to_string(), v.to_string());
        }
        Self {
            service: span_start.service,
            name: span_start.name,
            resource: span_start.resource,
            trace_id: span_start.trace_id.into(),
            span_id: span_start.id.into(),
            parent_id: span_start.parent_id.map(Into::into).unwrap_or(0),
            start: span_start.time as i64,
            duration: 0,
            error: 0,
            meta: meta,
            metrics: HashMap::new(),
            r#type: "type_todo".into(),
            meta_struct: HashMap::new(),
        }
    }
}

pub trait EventConsume {
    fn apply_into(self, span: &mut pb::Span);
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpanFinished {
    pub id: Id,
    pub trace_id: Id,
    pub parent_span_id: Option<Id>,
    pub duration: u64,
    pub time: u64,
}

impl EventConsume for SpanFinished {
    fn apply_into(self, span: &mut pb::Span) {
        span.duration = self.duration as i64;
    }
}

impl TransferHandles for SpanStart {
    fn move_handles<Transport: ddtelemetry::ipc::handles::HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.span_end_socket.move_handles(transport)
    }

    fn receive_handles<Transport: ddtelemetry::ipc::handles::HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.span_end_socket.receive_handles(transport)
    }
}

pub struct EventCollector {
    system_info: crate::config::SystemInfo,
    span_starts: HashMap<Id, HashMap<Id, Box<SpanStart>>>,
    cull_list: BTreeMap<u64, Vec<Id>>,
    events: HashMap<Id, Vec<Event>>,

    last_seen: HashMap<Id, u64>,
}

impl EventCollector {
    pub fn init(config: &crate::config::Config) -> Self {
        Self {
            system_info: config.system_info(),
            span_starts: Default::default(),
            cull_list: Default::default(),
            events: Default::default(),
            last_seen: Default::default(),
        }
    }

    pub async fn into_event_loop(
        mut self,
        payload_tx: Sender<Box<pb::TracerPayload>>,
        mut event_rx: Receiver<Event>,
    ) {
        let mut cull_interval = tokio::time::interval(Duration::from_secs(2));
        let mut gc_interval = tokio::time::interval(Duration::from_secs(20));

        loop {
            tokio::select! {
                Some(ev) = event_rx.recv() => {
                    match ev {
                        Event::StartSpan(s) => {
                            self.on_span_start(s);
                        },
                        Event::SpanFinished(s) => {
                            self.on_span_finished(s);
                        },
                        ev => {
                            self.on_unmatched_event(ev);
                        }
                    }
                }

                _ = cull_interval.tick() => {
                    let payloads = self.on_cull_interval();
                    for payload in payloads {
                        payload_tx.send(payload).await.ok(); //TODO - check errors - lots of error checking
                    }
                }
                _ = gc_interval.tick() => {
                    //TODO: gc leftover events
                }
            }
        }
    }

    fn on_unmatched_event(&mut self, ev: Event) {
        self.update_last_seen(ev.trace_id());

        self.store_event(ev);
    }

    fn on_cull_interval(&mut self) -> Vec<Box<TracerPayload>> {
        let now = now().as_secs();
        let mut keys_to_cull = Vec::with_capacity(10);
        let mut num_entries = 0;

        for (t, l) in &self.cull_list {
            if *t > now {
                break;
            }
            keys_to_cull.push(*t);
            num_entries += l.len();
        }
        let mut trace_ids = HashSet::with_capacity(num_entries);

        for key in keys_to_cull {
            if let Some(mut ids) = self.cull_list.remove(&key) {
                for id in ids {
                    trace_ids.insert(id);
                }
            }
        }
        if let Some(payload) = self.reassemble_payloads(&trace_ids) {
            vec![Box::new(payload)]
        } else {
            vec![] // TODO: this method should produce multiple tracer payloads in the future
        }
    }

    fn reassemble_payloads(&mut self, trace_ids: &HashSet<Id>) -> Option<TracerPayload> {
        let chunks: Vec<pb::TraceChunk> = trace_ids
            .iter()
            .flat_map(|id| self.reassemble_trace_chunk(id))
            .collect();

        if chunks.len() == 0 {
            return None;
        }

        Some(TracerPayload {
            container_id: "".into(),
            language_name: "shell".into(),
            language_version: "1.1".into(),
            tracer_version: "todo".into(),
            runtime_id: uuid::Uuid::new_v4().to_string(),
            chunks,
            tags: Default::default(),
            env: self.system_info.env.clone(),
            hostname: self.system_info.hostname.clone(),
            app_version: "6.6.6".into(),
        })
    }

    fn reassemble_trace_chunk(&mut self, id: &Id) -> Option<pb::TraceChunk> {
        let span_starts = self.span_starts.remove(id)?;
        let events = self.events.remove(id).unwrap_or_default();

        let mut spans: HashMap<Id, pb::Span> = HashMap::with_capacity(events.len() / 2);

        for (_, s) in span_starts {
            spans.insert(s.id.clone(), s.into());
        }
        let mut unmatched_cnt = 0;
        for ev in events {
            match ev {
                Event::SpanFinished(sf) => match spans.get_mut(&sf.id) {
                    Some(s) => sf.apply_into(s),
                    None => unmatched_cnt += 1,
                },
                Event::StartSpan(_) => {}
                Event::ReportError(_) => todo!(),
            }
        }
        if unmatched_cnt > 0 {
            println!("Unmatched events: {}", unmatched_cnt);
        }

        Some(pb::TraceChunk {
            priority: -1,
            origin: "web_todo".into(),
            spans: spans.into_values().collect(),
            tags: HashMap::new(),
            dropped_trace: false,
        })
    }

    fn update_last_seen(&mut self, trace_id: &Id) {
        self.last_seen.insert(trace_id.clone(), now().as_secs());
    }

    fn on_span_start(&mut self, span_start: Box<SpanStart>) {
        self.update_last_seen(&span_start.trace_id);

        if let Some(starts) = self.span_starts.get_mut(&span_start.trace_id) {
            starts.insert(span_start.id.clone(), span_start);
        } else {
            self.span_starts.insert(
                span_start.trace_id.clone(),
                HashMap::from([(span_start.id.clone(), span_start)]),
            );
        }
    }

    fn cull(&mut self, trace_id: &Id) {
        let time = now() + Duration::from_secs(2);
        if let Some(entry) = self.cull_list.get_mut(&time.as_secs()) {
            entry.push(trace_id.clone());
        } else {
            self.cull_list
                .insert(time.as_secs(), vec![trace_id.clone()]);
        }
    }

    fn store_event(&mut self, ev: Event) {
        let trace_id = ev.trace_id().clone();
        if let Some(evs) = self.events.get_mut(&trace_id) {
            evs.push(ev)
        } else {
            let mut vec = Vec::with_capacity(10);
            vec.push(ev);
            self.events.insert(trace_id, vec);
        }
    }

    fn on_span_finished(&mut self, span_finished: SpanFinished) {
        self.update_last_seen(&span_finished.trace_id);

        let parent_id = if let Some(id) = &span_finished.parent_span_id {
            id
        } else {
            self.cull(&span_finished.trace_id);
            self.store_event(Event::SpanFinished(span_finished));
            return;
        };

        if let Some(starts) = self.span_starts.get_mut(&span_finished.trace_id) {
            if !starts.contains_key(parent_id) {
                self.cull(&span_finished.trace_id);
            }
        }
        self.store_event(Event::SpanFinished(span_finished));
    }
}
