//! `npm run verify`: a fresh local sandbox, a freshly generated agent key, every claim asserted.
mod common;

use anyhow::{Context, Result, ensure};
use common::*;
use near_workspaces::types::{Gas, NearToken};
use serde_json::{Value, json};

#[tokio::test]
async fn verify() -> Result<()> {
    let wasm = wasm(None)?;
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let mut w = setup(worker, root, NEAR / 10, NearToken::from_near(10), &wasm, false).await?;
    main_flow(&mut w).await?;
    callback_replay(&mut w).await?;
    overlapping_requests(&mut w, &wasm).await?;
    callback_failure(&mut w).await?;

    print_attempts(&w.mandate).await?;
    println!("\nKeyDare verify: {} claims held, 0 broken.", w.ev.rows.len());
    Ok(())
}

/// The deployer key still exists before the lock (v0.9), so the mandate can call its own callback.
async fn callback_replay(w: &mut World<impl near_workspaces::Network + 'static>) -> Result<()> {
    let record = w.request("ghost-1").await?;
    let committed = w.committed().await?;
    for _ in 0..2 {
        let replay = w.mandate.call("on_transfer").args_json(json!({"index": record["index"]})).transact().await?;
        ensure!(replay.is_success(), "{replay:?}");
        ensure!(w.request("ghost-1").await? == record && w.committed().await? == committed, "callback replay changed state");
    }
    w.ev.held(
        "replaying the settlement callback changes nothing",
        "settlement",
        "two self-calls of on_transfer on the FAILED request: releases still 1, budget unchanged",
        None,
    );
    Ok(())
}

/// Two requests in flight at once may not together exceed the total: the charge is taken before
/// the transfer leaves, so the second request already sees the first one's charge.
async fn overlapping_requests(w: &mut World<impl near_workspaces::Network + 'static>, wasm: &[u8]) -> Result<()> {
    let agent = subaccount(&w.owner, "agent2", NearToken::from_near(1)).await?;
    let policy = json!({
        "version": 1, "payees": {w.vendor.id().as_str(): w.n(30).to_string()},
        "total_cap": w.n(6).to_string(), "approval_threshold": w.n(6).to_string(),
    });
    let (mandate, _) = deploy_mandate(&w.owner, "mandate2", NearToken::from_near(10), wasm, agent.id(), policy).await?;
    let vendor_start = w.balance(w.vendor.id()).await?;
    let mut sent = vec![];
    for id in ["overlap-a", "overlap-b"] {
        sent.push(
            agent
                .call(mandate.id(), "request_spend")
                .args_json(json!({"request_id": id, "payee": w.vendor.id(), "amount": w.n(4).to_string(), "policy_version": 1}))
                .gas(Gas::from_tgas(100))
                .transact_async()
                .await?,
        );
    }
    let mut results = vec![];
    for status in sent {
        results.push(status.await?);
    }
    let mut outcomes = vec![];
    for result in &results {
        outcomes.push(result.clone().json::<Value>()?);
    }
    let (paid, denied) = match (&outcomes[0]["status"], &outcomes[1]["status"]) {
        (a, b) if a == "SETTLED" && b == "DENIED" => (0, 1),
        (a, b) if a == "DENIED" && b == "SETTLED" => (1, 0),
        _ => anyhow::bail!("expected one settled and one denied: {outcomes:?}"),
    };
    ensure!(outcomes[denied]["reason"] == "TOTAL_CAP", "{outcomes:?}");
    ensure!(w.balance(w.vendor.id()).await? - vendor_start == w.n(4), "both overlapping requests paid");

    let denied_at = w.height(results[denied].receipt_outcomes()[0].block_hash).await?;
    let callback = results[paid]
        .receipt_outcomes()
        .iter()
        .find(|o| o.logs.iter().any(|l| l.contains("\"event\":\"settled\"")))
        .context("no settlement callback")?;
    let settled_at = w.height(callback.block_hash).await?;
    ensure!(denied_at < settled_at, "requests did not overlap: denied at {denied_at}, settled at {settled_at}");
    w.ev.held(
        "two requests in flight at once cannot exceed the total cap",
        "contract",
        &format!("one SETTLED, one DENIED TOTAL_CAP at block {denied_at} while the first transfer was unsettled (callback at block {settled_at}); vendor +4 steps once"),
        Some(&results[denied]),
    );
    Ok(())
}

/// A test-only build whose callback always panics: the transfer leaves, the charge stays frozen and
/// visible as PENDING, and nothing is refunded or resent.
async fn callback_failure(w: &mut World<impl near_workspaces::Network + 'static>) -> Result<()> {
    let broken = wasm(Some("inject-callback-failure"))?;
    let agent = subaccount(&w.owner, "agent3", NearToken::from_near(1)).await?;
    let policy = policy(w.vendor.id(), w.hosting.id(), &w.ghost, w.step);
    let (mandate, _) = deploy_mandate(&w.owner, "mandate3", NearToken::from_near(10), &broken, agent.id(), policy).await?;
    let vendor_start = w.balance(w.vendor.id()).await?;
    let request = |id: &str| {
        agent
            .call(mandate.id(), "request_spend")
            .args_json(json!({"request_id": id, "payee": w.vendor.id(), "amount": w.n(3).to_string(), "policy_version": 1}))
            .gas(Gas::from_tgas(100))
            .transact()
    };
    let first = request("frozen-1").await?;
    ensure!(format!("{:?}", first.receipt_failures()).contains("INJECTED_CALLBACK_FAILURE"), "{first:?}");
    let view = |method: &'static str, args: Value| {
        let mandate = mandate.clone();
        async move { Ok::<Value, anyhow::Error>(mandate.view(method).args_json(args).await?.json()?) }
    };
    let record = view("get_request", json!({"request_id": "frozen-1"})).await?;
    ensure!(record["status"] == "PENDING" && record["releases"] == 0, "{record}");
    ensure!(view("mandate", json!({})).await?["committed"] == w.n(3).to_string());
    let retry: Value = request("frozen-1").await?.json()?;
    ensure!(retry["status"] == "DENIED" && retry["reason"] == "REPLAY", "{retry}");
    ensure!(w.balance(w.vendor.id()).await? - vendor_start == w.n(3), "paid more than once");
    ensure!(view("mandate", json!({})).await?["committed"] == w.n(3).to_string());
    w.ev.held(
        "if the settlement callback fails, the charge stays frozen and visible; no refund, no resend",
        "settlement",
        "test-only build (feature inject-callback-failure): vendor +3 steps once, record PENDING, committed 3, retry DENIED REPLAY",
        Some(&first),
    );
    Ok(())
}

/// What we deploy (and what testnet runs) is what this source builds to.
#[test]
fn prebuilt_matches_source() -> Result<()> {
    let built = sha256_hex(&wasm(None)?);
    let prebuilt = sha256_hex(&std::fs::read("res/keydare.wasm")?);
    let recorded = std::fs::read_to_string("res/keydare.wasm.sha256")?;
    ensure!(
        built == prebuilt && recorded.split_whitespace().next() == Some(built.as_str()),
        "rebuilt wasm {built} differs from res/keydare.wasm {prebuilt} or res/keydare.wasm.sha256 {recorded}"
    );
    println!("res/keydare.wasm is byte-identical to this source's build: sha256 {built}");
    Ok(())
}

/// The deployed code exposes no way to deploy code, add keys or edit the policy.
#[test]
fn no_admin_surface() -> Result<()> {
    let mut exports = exports(&wasm(None)?)?;
    exports.sort();
    // contract_source_metadata is the NEP-330 view near-sdk adds to every contract.
    let expected = ["approve", "attempts", "contract_source_metadata", "get_request", "mandate", "new", "on_transfer", "reject", "request_spend"];
    ensure!(exports == expected, "unexpected exported methods: {exports:?}");
    let source = std::fs::read_to_string("src/lib.rs")?;
    for action in ["deploy_contract", "add_full_access_key", "add_access_key", "delete_key", "delete_account", "create_account", "function_call", "stake", "env::storage_write", "env::promise_batch"] {
        ensure!(!source.contains(action), "src/lib.rs contains {action}");
    }
    ensure!(source.matches("Promise::new(").count() == 1 && source.contains("Promise::new(attempt.request.payee.clone())\n            .transfer("));
    println!("exported methods: {exports:?}; the only outgoing action is a transfer to the request's payee");
    Ok(())
}

/// Function names in the wasm export section.
fn exports(wasm: &[u8]) -> Result<Vec<String>> {
    fn leb(bytes: &[u8], at: &mut usize) -> usize {
        let (mut value, mut shift) = (0, 0);
        loop {
            let byte = bytes[*at];
            *at += 1;
            value |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
            shift += 7;
        }
    }
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let size = leb(wasm, &mut at);
        if id != 7 {
            at += size;
            continue;
        }
        let mut names = vec![];
        for _ in 0..leb(wasm, &mut at) {
            let len = leb(wasm, &mut at);
            let name = String::from_utf8(wasm[at..at + len].to_vec())?;
            at += len;
            let kind = wasm[at];
            at += 1;
            leb(wasm, &mut at);
            if kind == 0 {
                names.push(name);
            }
        }
        return Ok(names);
    }
    anyhow::bail!("no export section")
}
