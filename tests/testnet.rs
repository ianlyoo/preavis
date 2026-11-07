//! `npm run execute-testnet`: the mandate scenarios on NEAR testnet, with receipt links.
//! Creates a fresh faucet-funded owner (random key, saved under the ignored .credentials/) unless
//! PREAVIS_OWNER_CREDENTIALS points at a near-cli credential file of a funded testnet account.
mod common;

use anyhow::{Context, Result};
use common::*;
use near_workspaces::Account;
use near_workspaces::types::{KeyType, NearToken, SecretKey};
use serde_json::json;

#[tokio::test]
#[ignore = "uses NEAR testnet and the faucet; run with `npm run execute-testnet`"]
async fn execute_testnet() -> Result<()> {
    let rpc = std::env::var("PREAVIS_TESTNET_RPC").unwrap_or_else(|_| "https://test.rpc.fastnear.com".into());
    let worker = near_workspaces::testnet().rpc_addr(&rpc).await?;
    let owner = match std::env::var("PREAVIS_OWNER_CREDENTIALS") {
        Ok(path) => Account::from_file(&path, &worker).with_context(|| path)?,
        Err(_) => {
            let id = format!("preavis-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs());
            worker
                .create_root_account_subaccount(id.parse()?, SecretKey::from_random(KeyType::ED25519))
                .await
                .context("faucet account creation (helper.testnet.near.org) failed; see README for PREAVIS_OWNER_CREDENTIALS")?
                .into_result()?
        }
    };
    let keys = format!(".credentials/testnet/{}", owner.id());
    std::fs::create_dir_all(&keys)?;
    owner.store_credentials(&keys).await?;
    println!("owner {} (credentials saved under {keys}/, not printed)", owner.id());

    let wasm = contract_wasm()?;
    let mut w = setup(worker, owner, NEAR / 10_000, NearToken::from_near(5), &wasm, true).await?;
    w.treasury.store_credentials(&keys).await?;
    main_flow(&mut w).await?;
    let offline = w.agent_offline_key.clone().context("agent not handed over")?;
    let agent_admin = near_workspaces::Account::from_secret_key(w.agent.id().clone(), offline.clone(), &w.worker);
    agent_admin.store_credentials(&keys).await?;
    w.lock_and_spend().await?;
    print_attempts(&w.mandate).await?;

    // What the observer re-reads later: each transaction's receiver and actions, and the end state.
    for row in w.ev.rows.iter_mut() {
        if let (Some(tx), Some(signer)) = (row["tx"].as_str(), row["signer"].as_str()) {
            let sent = transaction(&w.worker, tx, signer).await?;
            row["receiver"] = sent["receiver"].clone();
            row["actions"] = sent["actions"].clone();
        }
    }
    let mut balances = serde_json::Map::new();
    for account in [&w.vendor, &w.hosting, &w.attacker, &w.treasury] {
        balances.insert(account.id().to_string(), json!(w.balance(account.id()).await?.to_string()));
    }
    let attempts: serde_json::Value = w.mandate.view("attempts").args_json(json!({"from": 0, "limit": 100})).await?.json()?;

    let record = json!({
        "network": "testnet",
        "rpc": rpc,
        "recorded_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        "owner": w.owner.id(),
        "mandate": w.mandate.id(),
        "agent": w.agent.id(),
        "agent_public_key": w.agent.secret_key().public_key().to_string(),
        "agent_offline_public_key": offline.public_key().to_string(),
        "vendor": w.vendor.id(),
        "hosting": w.hosting.id(),
        "attacker": w.attacker.id(),
        "treasury": w.treasury.id(),
        "ghost": w.ghost,
        "code_sha256": sha256_hex(&wasm),
        "step_yocto": w.step.to_string(),
        "claims": w.ev.rows,
        "end_state": {"mandate": w.summary().await?, "attempts": attempts, "balances": balances},
    });
    std::fs::write("target/preavis-testnet-run.json", serde_json::to_string_pretty(&record)?)?;
    println!("\nPreavis testnet: {} claims held, 0 broken. Run record: target/preavis-testnet-run.json", w.ev.rows.len());
    Ok(())
}
