//! `npm run agent`: the agent works through `agent/inbox/` once, in the local office sandbox.
#[path = "../tests/common/mod.rs"]
mod common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::office::run_agent(&common::office::office_dir()).await
}
