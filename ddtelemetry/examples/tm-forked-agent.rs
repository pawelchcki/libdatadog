
fn main() -> anyhow::Result<()> {
    let (send, recv) = ipc_channel::platform::channel()?;
    Ok(())
}
