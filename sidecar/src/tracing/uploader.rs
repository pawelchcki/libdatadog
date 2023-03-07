use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use datadog_trace_protobuf::{
    pb::{AgentPayload, TraceChunk, TracerPayload},
    prost::Message,
};
use hyper::{Method, Request};
use tokio::sync::mpsc::Receiver;

use crate::data::v04;

pub struct Uploader {
    tracing_config: crate::config::TracingConfig,
    system_info: crate::config::SystemInfo,
    client: ddcommon::HttpClient,
}

impl Uploader {
    pub fn init(cfg: &crate::config::Config) -> Self {
        let client = hyper::Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build(ddcommon::connector::Connector::new());

        Self {
            tracing_config: cfg.tracing_config(),
            system_info: cfg.system_info(),
            client,
        }
    }

    fn find_top_level(chunk: &mut TraceChunk) -> Option<&mut datadog_trace_protobuf::pb::Span> {
        let mut span_ids = HashSet::with_capacity(chunk.spans.len());
        for s in &chunk.spans {
            span_ids.insert(s.span_id);
        }

        for s in &mut chunk.spans {
            if !span_ids.contains(&s.parent_id) {
                return Some(s);
            }
        }

        None
    }

    pub async fn submit(&self, mut payloads: Vec<TracerPayload>) -> anyhow::Result<()> {
        let req = match self.tracing_config.protocol {
            crate::config::TracingProtocol::BackendProtobufV01 => {
                let mut tags = HashMap::new();
                tags.insert("some_tag".into(), "value".into());

                for head_span in payloads
                    .iter_mut()
                    .flat_map(|f| f.chunks.iter_mut().flat_map(|t| Self::find_top_level(t)))
                {
                    head_span.metrics.insert("_dd.agent_psr".into(), 1.0);
                    head_span.metrics.insert("_sample_rate".into(), 1.0);
                    head_span
                        .metrics
                        .insert("_sampling_priority_v1".into(), 1.0);
                    head_span.metrics.insert("_top_level".into(), 1.0);
                }

                let payload = AgentPayload {
                    host_name: self.system_info.hostname.clone(),
                    env: self.system_info.env.clone(),
                    tracer_payloads: payloads,
                    tags, //TODO: parse DD_TAGS
                    agent_version: "libdatadog".into(),
                    target_tps: 60.0,
                    error_tps: 60.0,
                };

                let mut req_builder = Request::builder()
                    .method(Method::POST)
                    .header("Content-Type", "application/x-protobuf")
                    .header("X-Datadog-Reported-Languages", "rust,TODO")
                    .uri(&self.tracing_config.url);

                for (key, value) in &self.tracing_config.http_headers {
                    req_builder = req_builder.header(key, value);
                }
                let data = payload.encode_to_vec();

                req_builder.body(data.into())?
            }
            crate::config::TracingProtocol::AgentV04 => {
                let data: Vec<v04::Trace> = payloads
                    .iter()
                    .flat_map(|p| p.chunks.iter().map(|c| c.into()))
                    .collect();
                let data = v04::Payload { traces: data };
                let data = serde_json::to_vec(&data)?;

                // TODO: fix msgpack serialization
                // let data = rmp_serde::to_vec(&data)?;

                let mut req_builder = Request::builder()
                    .method(Method::POST)
                    .header("Content-Type", "application/json")
                    .uri(&self.tracing_config.url);

                for (key, value) in &self.tracing_config.http_headers {
                    req_builder = req_builder.header(key, value);
                }
                req_builder.body(data.into())?
            }
        };

        let mut resp = self.client.request(req).await?;
        let _data = hyper::body::to_bytes(resp.body_mut()).await?;
        Ok(())
    }

    pub async fn into_event_loop(self, mut rx: Receiver<Box<TracerPayload>>) {
        let mut payloads = vec![];
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                Some(d) = rx.recv() => {
                    payloads.push(d);
                }

                _ = interval.tick() => {
                    if payloads.is_empty() {
                        continue
                    }
                    match self.submit(payloads.drain(..).map(|p| *p).collect()).await {
                        Ok(()) => {eprintln!("submitted")},
                        Err(e) => {eprintln!("{:?}", e)}
                    }
                }
            }
        }
    }
}
