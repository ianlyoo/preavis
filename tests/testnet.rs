//! `npm run execute-testnet`: the mandate scenarios on NEAR testnet, with receipt links.
//! Creates a fresh faucet-funded owner (random key, saved under the ignored .credentials/) unless
//! KEYDARE_OWNER_CREDENTIALS points at a near-cli credential file of a funded testnet account.
mod common;

use anyhow::{Context, Result};
use common::*;
use near_workspaces::Account;
use near_workspaces::types::{KeyType, NearToken, SecretKey};
use serde_json::json;

#[tokio::test]
#[ignore = "uses NEAR testnet and the faucet; run with `npm run execute-testnet`"]
async fn execute_testnet() -> Result<()> {
    let rpc = std::env::var("KEYDARE_TESTNET_RPC").unwrap_or_else(|_| "https://archival-rpc.testnet.near.org".into());
    let worker = near_workspaces::testnet().rpc_addr(&rpc).await?;
    let owner = match std::env::var("KEYDARE_OWNER_CREDENTIALS") {
        Ok(path) => Account::from_file(&path, &worker).with_context(|| path)?,
        Err(_) => {
            let id = format!("keydare-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs());
            worker
                .create_root_account_subaccount(id.parse()?, SecretKey::from_random(KeyType::ED25519))
                .await
                .context("faucet account creation (helper.testnet.near.org) failed; see README for KEYDARE_OWNER_CREDENTIALS")?
                .into_result()?
        }
    };
    let keys = format!(".credentials/testnet/{}", owner.id());
    std::fs::create_dir_all(&keys)?;
    owner.store_credentials(&keys).await?;
    println!("owner {} (credentials saved under {keys}/, not printed)", owner.id());

    let wasm = std::fs::read("res/keydare.wasm")?;
    let mut w = setup(worker, owner, NEAR / 10_000, NearToken::from_near(5), &wasm, true).await?;
    w.mandate.as_account().store_credentials(&keys).await?;
    main_flow(&mut w).await?;
    print_attempts(&w.mandate).await?;

    let record = json!({
        "network": "testnet",
        "rpc": rpc,
        "recorded_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        "owner": w.owner.id(),
        "mandate": w.mandate.id(),
        "agent": w.agent.id(),
        "agent_public_key": w.agent.secret_key().public_key().to_string(),
        "code_sha256": sha256_hex(&wasm),
        "step_yocto": w.step.to_string(),
        "claims": w.ev.rows,
    });
    std::fs::write("target/keydare-testnet-run.json", serde_json::to_string_pretty(&record)?)?;
    println!("\nKeyDare testnet: {} claims held, 0 broken. Run record: target/keydare-testnet-run.json", w.ev.rows.len());
    Ok(())
}
