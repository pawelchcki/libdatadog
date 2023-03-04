pub mod config;
pub mod connections;
pub mod data;
pub mod ipc;
pub mod ipc_agent;
pub mod libc_main;
pub mod mini_agent;
pub mod pipes;
pub mod sidecar;
pub mod tracing;

mod rust_tracing {
    pub fn enable_tracing() -> anyhow::Result<()> {
        let subscriber = tracing_subscriber::fmt();
        subscriber.with_writer(std::io::stderr).init();
        Ok(())
    }
}
