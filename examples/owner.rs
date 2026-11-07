//! `npm run owner -- list | show <id> | approve <id> | reject <id>`: the owner's side, separate
//! from the agent.
#[path = "../tests/common/mod.rs"]
mod common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    common::office::run_owner(&common::office::office_dir(), &args).await
}
