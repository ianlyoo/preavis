//! `npm run challenge-deploy`: a fresh, locked testnet mandate meant as a standing public challenge.
//! Whoever holds the agent's function-call key may call `request_spend` as often as they like; the
//! question is whether they can make the mandate pay anyone but its two allowlisted payees.
//!
//! Exhaustion is bounded per day: each period of `PERIOD` blocks (24 h at testnet's ~0.6 s a
//! block) takes at most `MAX_PAYMENTS` requests and stores `MAX_DENIALS` denials; past that the
//! mandate is EXHAUSTED until `resets_at`, and the next day starts fresh. The init reserve covers
//! storage for every period up to `expires_at`, so a flood can't exhaust storage either.
//!
//! The key's secret half is written only to the ignored `.credentials/testnet/challenge/<label>/`.
//! The public record, `testnet/challenge.json`, holds account names, public keys and transactions.
//! Rotation: run again with a new `PREAVIS_CHALLENGE_LABEL`; see README, "Challenge".
//! Environment: PREAVIS_OWNER_CREDENTIALS (else a faucet account), PREAVIS_FUNDERS, PREAVIS_CHALLENGE_LABEL
//! (default c1), PREAVIS_CHALLENGE_UNTIL (unix time, default 2026-11-27 00:00 KST),
//! PREAVIS_CHALLENGE_ALLOWANCE_NEAR (default 8).
mod common;

use anyhow::{Context, Result, ensure};
use common::*;
use near_workspaces::Account;
use near_workspaces::types::{AccessKey, Gas, KeyType, NearToken, SecretKey};
use serde_json::{Value, json};
use std::io::Write;

/// 24 hours of testnet blocks at ~0.6 s each.
const PERIOD: u64 = 144_000;
const MAX_PAYMENTS: u32 = 10;
const MAX_DENIALS: u32 = 40;
/// The agent's own budget per day, and the caps; all far below what an attacker could want.
const PER_DAY: u128 = NEAR / 20;
const PAYEE_CAP: u128 = NEAR;
const TOTAL_CAP: u128 = NEAR * 6 / 5;

#[tokio::test]
#[ignore = "deploys on NEAR testnet; run with `npm run challenge-deploy`"]
async fn deploy_challenge() -> Result<()> {
    let rpc = std::env::var("PREAVIS_TESTNET_RPC")
        .unwrap_or_else(|_| "https://test.rpc.fastnear.com".into());
    let worker = near_workspaces::testnet().rpc_addr(&rpc).await?;
    // The owner: a near-cli credential file, or a fresh faucet account saved under .credentials/.
    let owner = match std::env::var("PREAVIS_OWNER_CREDENTIALS") {
        Ok(path) => Account::from_file(&path, &worker).with_context(|| path)?,
        Err(_) => {
            let id = format!(
                "preavis-challenge-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs()
            );
            let owner = worker
                .create_root_account_subaccount(
                    id.parse()?,
                    SecretKey::from_random(KeyType::ED25519),
                )
                .await
                .context("faucet account creation (helper.testnet.near.org) failed")?
                .into_result()?;
            let dir = format!(".credentials/testnet/{}", owner.id());
            std::fs::create_dir_all(&dir)?;
            owner.store_credentials(&dir).await?;
            println!(
                "owner {} (credentials saved under {dir}/, not printed)",
                owner.id()
            );
            owner
        }
    };
    // Optional top-up from other testnet accounts you hold: comma-separated credential files.
    if let Ok(funders) = std::env::var("PREAVIS_FUNDERS") {
        for path in funders.split(',').filter(|p| !p.is_empty()) {
            let funder = Account::from_file(path, &worker).with_context(|| path.to_string())?;
            let spare = worker
                .view_account(funder.id())
                .await?
                .balance
                .as_yoctonear()
                .saturating_sub(NEAR / 10);
            funder
                .transfer_near(owner.id(), NearToken::from_yoctonear(spare))
                .await?
                .into_result()?;
            println!(
                "topped up from {}: {:.3} NEAR",
                funder.id(),
                spare as f64 / NEAR as f64
            );
        }
    }
    let label = std::env::var("PREAVIS_CHALLENGE_LABEL").unwrap_or_else(|_| "c1".into());
    // Default end: 2026-11-27 00:00 KST, two days after the judging window closes.
    let until: u64 = std::env::var("PREAVIS_CHALLENGE_UNTIL")
        .map(|s| s.parse())
        .unwrap_or(Ok(1_795_705_200))?;
    let allowance = NearToken::from_near(
        std::env::var("PREAVIS_CHALLENGE_ALLOWANCE_NEAR")
            .map(|s| s.parse())
            .unwrap_or(Ok(8))?,
    );
    let started = std::time::Instant::now();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    ensure!(
        until > now + 86_400,
        "PREAVIS_CHALLENGE_UNTIL is less than a day away"
    );
    let keys = format!(".credentials/testnet/challenge/{label}");
    std::fs::create_dir_all(&keys)?;

    let small = NearToken::from_millinear(50);
    let vendor = subaccount(&owner, &format!("{label}-vendor"), small).await?;
    let hosting = subaccount(&owner, &format!("{label}-hosting"), small).await?;
    let treasury = subaccount(&owner, &format!("{label}-treasury"), small).await?;
    let agent = subaccount(
        &owner,
        &format!("{label}-agent"),
        allowance.saturating_add(NearToken::from_millinear(100)),
    )
    .await?;
    for account in [&vendor, &hosting, &treasury, &agent] {
        account.store_credentials(&keys).await?;
    }

    let head = worker.view_block().await?;
    let (height, block_ms) = (head.height(), 600u64);
    let expires_at = height + (until - now) * 1000 / block_ms;
    let periods = (expires_at - height).div_ceil(PERIOD);
    let policy = json!({
        "version": 1,
        "payees": {vendor.id().as_str(): PAYEE_CAP.to_string(), hosting.id().as_str(): PAYEE_CAP.to_string()},
        "total_cap": TOTAL_CAP.to_string(), "approval_threshold": PER_DAY.to_string(),
        "period_blocks": PERIOD, "expires_at": expires_at,
        "max_payments": MAX_PAYMENTS, "max_denials": MAX_DENIALS,
    });
    let wasm = contract_wasm()?;
    // Storage: the code, the state, and 500 bytes for every record in every period; 10^19 yocto a
    // byte. Plus the total cap and 0.1 NEAR of slack for the gas the contract burns.
    let records = u128::from(MAX_PAYMENTS + MAX_DENIALS) * u128::from(periods);
    let bytes = wasm.len() as u128 + 1_000 + 2 * 200 + 500 * records;
    let balance = bytes * 10u128.pow(19) + TOTAL_CAP + NEAR / 10;
    println!(
        "challenge {label}: {periods} periods of {PERIOD} blocks until block {expires_at} (~{until} unix); {records} records reserved; mandate balance {} NEAR",
        near(balance)
    );
    let (mandate, init) = deploy_mandate(
        &owner,
        &label,
        NearToken::from_yoctonear(balance),
        &wasm,
        agent.id(),
        treasury.id(),
        policy.clone(),
    )
    .await?;

    let key = SecretKey::from_random(KeyType::ED25519);
    let handed = agent
        .batch(agent.id())
        .add_key(
            key.public_key(),
            AccessKey::function_call_access(mandate.id(), &["request_spend"], Some(allowance)),
        )
        .transact()
        .await?;
    ensure!(handed.is_success(), "{handed:?}");
    let offline = agent.secret_key().clone();
    let locked = mandate
        .as_account()
        .batch(mandate.id())
        .delete_key(mandate.as_account().secret_key().public_key())
        .transact()
        .await?;
    ensure!(locked.is_success(), "{locked:?}");
    ensure!(
        worker.view_access_keys(mandate.id()).await?.is_empty(),
        "mandate still has a key"
    );

    // The secret half: only here, mode 0600, never printed.
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
    file.open(format!("{keys}/agent-fc-key"))
        .with_context(|| format!("{keys}/agent-fc-key exists: use a new PREAVIS_CHALLENGE_LABEL"))?
        .write_all(key.to_string().as_bytes())?;

    // Calibration, with the public key itself: one denied request at the most gas an attacker can
    // attach, to measure what a call costs the key's allowance.
    let player = Account::from_secret_key(agent.id().clone(), key.clone(), &worker);
    let allowance_left = |worker: near_workspaces::Worker<near_workspaces::network::Testnet>,
                          id: near_workspaces::AccountId,
                          pk| async move {
        let k = worker.view_access_key(&id, &pk).await?;
        match k.permission {
            near_workspaces::types::AccessKeyPermission::FunctionCall(p) => {
                Ok::<u128, anyhow::Error>(p.allowance.map(|a| a.as_yoctonear()).unwrap_or(0))
            }
            _ => anyhow::bail!("not a function-call key"),
        }
    };
    let before = allowance_left(worker.clone(), agent.id().clone(), key.public_key()).await?;
    let probe = player
        .call(mandate.id(), "request_spend")
        .args_json(json!({"request_id": "calibration", "payee": owner.id(), "amount": "1", "policy_version": 1, "invoice": "calibration"}))
        .gas(Gas::from_tgas(300))
        .transact()
        .await?;
    let probe_attempt: Value = probe.clone().json()?;
    ensure!(probe_attempt["reason"] == "UNLISTED", "{probe_attempt}");
    // The key is charged the prepaid gas at a pessimistic price while the call is in flight; the
    // unused part comes back with the refund receipt a few blocks later.
    let in_flight =
        before - allowance_left(worker.clone(), agent.id().clone(), key.public_key()).await?;
    let mut per_call = in_flight;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        per_call =
            before - allowance_left(worker.clone(), agent.id().clone(), key.public_key()).await?;
        if per_call < in_flight / 10 {
            break;
        }
    }

    let account = worker.view_account(mandate.id()).await?;
    let mut start_balances = serde_json::Map::new();
    for id in [vendor.id(), hosting.id(), treasury.id()] {
        start_balances.insert(
            id.to_string(),
            json!(
                worker
                    .view_account(id)
                    .await?
                    .balance
                    .as_yoctonear()
                    .to_string()
            ),
        );
    }
    let tx = |r: &near_workspaces::result::ExecutionFinalResult| {
        r.outcome().transaction_hash.to_string()
    };
    let record = json!({
        "_what": "A standing challenge on NEAR testnet: the agent's function-call key may call request_spend as often as anyone likes. Public record; the key's secret half is not in this repository.",
        "network": "testnet",
        "label": label,
        "owner": owner.id(),
        "mandate": mandate.id(),
        "agent": agent.id(),
        "agent_public_key": key.public_key().to_string(),
        "agent_offline_public_key": offline.public_key().to_string(),
        "allowlisted": [vendor.id(), hosting.id()],
        "refund_to": treasury.id(),
        "policy": policy,
        "code_sha256": sha256_hex(&wasm),
        "created_at_unix": now,
        "created_at_height": height,
        "expires_at_unix_estimate": until,
        "assumed_block_ms": block_ms,
        "mandate_start_balance": account.balance.as_yoctonear().to_string(),
        "start_balances": start_balances,
        "allowance_yocto": allowance.as_yoctonear().to_string(),
        "calibration": {"tx": tx(&probe), "gas_attached_tgas": 300, "allowance_in_flight_yocto": in_flight.to_string(), "allowance_used_yocto": per_call.to_string()},
        "transactions": {"init": tx(&init), "hand_over": tx(&handed), "lock": tx(&locked)},
        "deploy_seconds": started.elapsed().as_secs(),
    });
    std::fs::create_dir_all("testnet")?;
    std::fs::write(
        "testnet/challenge.json",
        serde_json::to_string_pretty(&record)? + "\n",
    )?;
    println!(
        "challenge {label} deployed and locked in {} s: mandate {}, agent {} (function-call key {}, allowance {} NEAR; one denied 300 Tgas call held {} NEAR of it in flight and used {} NEAR after the refund). Secret half: {keys}/agent-fc-key (not printed). Record: testnet/challenge.json",
        started.elapsed().as_secs(),
        mandate.id(),
        agent.id(),
        key.public_key(),
        near(allowance.as_yoctonear()),
        near(in_flight),
        near(per_call),
    );
    Ok(())
}

fn near(yocto: u128) -> String {
    format!("{:.6}", yocto as f64 / NEAR as f64)
}
