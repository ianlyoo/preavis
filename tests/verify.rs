//! `npm run verify`: a fresh local sandbox, a freshly generated agent key, every claim asserted
//! against the committed `res/preavis.wasm` (or a native build of `src/` with PREAVIS_WASM=source).
mod common;

use anyhow::{Context, Result, ensure};
use near_workspaces::AccountId;
use common::*;
use near_workspaces::types::{Gas, NearToken};
use serde_json::{Value, json};

#[tokio::test]
async fn verify() -> Result<()> {
    let wasm = contract_wasm()?;
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let mut w = setup(worker, root, NEAR / 10, NearToken::from_near(10), &wasm, false).await?;
    main_flow(&mut w).await?;
    callback_replay(&mut w).await?;
    w.lock_and_spend().await?;
    overlapping_requests(&mut w, &wasm).await?;
    callback_failure(&mut w).await?;
    bounded_storage(&mut w, &wasm).await?;
    close(&mut w).await?;
    w.fire_agent().await?;

    print_attempts(&w.mandate).await?;
    println!("\nPreavis verify: {} claims held, 0 broken.", w.ev.rows.len());
    Ok(())
}

/// Runs before the lock: only the deployer's key can make the mandate call its own callback.
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
        "max_payments": 10, "max_denials": 10,
    });
    let (mandate, _) = deploy_mandate(&w.owner, "mandate2", NearToken::from_near(10), wasm, agent.id(), w.treasury.id(), policy).await?;
    let vendor_start = w.balance(w.vendor.id()).await?;
    let mut sent = vec![];
    for id in ["overlap-a", "overlap-b"] {
        sent.push(
            agent
                .call(mandate.id(), "request_spend")
                .args_json(w.args(id, w.vendor.id(), 4, 1))
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
    let (mandate, _) = deploy_mandate(&w.owner, "mandate3", NearToken::from_near(10), &broken, agent.id(), w.treasury.id(), policy).await?;
    let vendor_start = w.balance(w.vendor.id()).await?;
    let request = |id: &str| {
        agent
            .call(mandate.id(), "request_spend")
            .args_json(w.args(id, w.vendor.id(), 3, 1))
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

/// Hostile requests can't grow storage without bound: a mandate takes at most `max_payments`
/// payments and stores at most `max_denials` denials, and init refuses a balance that doesn't
/// cover the total cap plus storage for all of them.
async fn bounded_storage(w: &mut World<impl near_workspaces::Network + 'static>, wasm: &[u8]) -> Result<()> {
    let agent = subaccount(&w.owner, "agent4", NearToken::from_near(1)).await?;
    let policy = |total: u128| json!({
        "version": 1, "payees": {w.vendor.id().as_str(): w.n(30).to_string(), w.ghost.as_str(): w.n(10).to_string()},
        "total_cap": total.to_string(), "approval_threshold": w.n(10).to_string(), "max_payments": 3, "max_denials": 3,
    });
    let poor = subaccount(&w.owner, "poor", NearToken::from_near(3)).await?.deploy(wasm).await?.into_result()?;
    let refused = poor.call("new").args_json(json!({"owner": w.owner.id(), "agent": agent.id(), "refund_to": w.treasury.id(), "policy": policy(NEAR)})).transact().await?;
    ensure!(failed_with(&refused, "BALANCE_BELOW_CAP_AND_STORAGE"), "{refused:?}");

    let (mandate, _) = deploy_mandate(&w.owner, "mandate4", NearToken::from_near(10), wasm, agent.id(), w.treasury.id(), policy(w.n(40))).await?;
    let storage = || async { Ok::<u64, anyhow::Error>(w.worker.view_account(mandate.id()).await?.storage_usage) };
    let long = |tag: &str| format!("{tag}-{}", "x".repeat(63 - tag.len()));
    let mut biggest = 0;
    let send = |id: String, payee: AccountId, steps: u128| {
        let call = agent
            .call(mandate.id(), "request_spend")
            .args_json(json!({"request_id": id, "payee": payee, "amount": w.n(steps).to_string(), "policy_version": 1, "invoice": "f".repeat(64)}))
            .gas(Gas::from_tgas(100));
        async move { Ok::<Value, anyhow::Error>(call.transact().await?.json()?) }
    };
    let vendor = w.vendor.id().clone();
    let expected = [
        ("p1", vendor.clone(), 1, "SETTLED"),
        ("p2", w.ghost.clone(), 1, "FAILED"),
        ("p3", vendor.clone(), 20, "AWAITING_APPROVAL"),
        ("p4", vendor.clone(), 1, "PAYMENT_LIMIT"),
        ("d1", w.attacker.id().clone(), 1, "UNLISTED"),
        ("d2", w.attacker.id().clone(), 1, "UNLISTED"),
    ];
    for (tag, payee, steps, outcome) in expected {
        let before = storage().await?;
        let mut attempt = send(long(tag), payee, steps).await?;
        if attempt["status"] == "PENDING" {
            attempt = mandate.view("get_request").args_json(json!({"request_id": long(tag)})).await?.json()?;
        }
        ensure!(attempt["status"] == outcome || attempt["reason"] == outcome, "{tag}: {attempt}");
        ensure!(!attempt["index"].is_null(), "{tag} not stored");
        biggest = biggest.max(storage().await? - before);
    }
    ensure!(biggest <= 500, "one record grew storage by {biggest} bytes, more than the 500 the reserve assumes");
    let full = storage().await?;
    for n in 0..5 {
        let attempt = send(long(&format!("extra{n}")), w.attacker.id().clone(), 1).await?;
        ensure!(attempt["reason"] == "UNLISTED" && attempt["index"].is_null(), "{attempt}");
    }
    ensure!(storage().await? == full, "storage grew past the limits");
    let approved = w.owner.call(mandate.id(), "approve")
        .args_json(json!({"request_id": long("p3"), "payee": vendor, "amount": w.n(20).to_string(), "policy_version": 1, "invoice": "f".repeat(64)}))
        .gas(Gas::from_tgas(100)).transact().await?;
    ensure!(approved.is_success() && approved.receipt_failures().is_empty(), "{approved:?}");
    let summary: Value = mandate.view("mandate").await?.json()?;
    ensure!(summary["payments"] == 3 && summary["denials"] == 3 && summary["denials_not_stored"] == 5, "{summary}");
    ensure!(storage().await? == full, "settling grew storage");
    w.ev.held(
        "hostile requests can't grow storage without bound, and the held request still settles",
        "contract",
        &format!("limits 3 payments + 3 stored denials; 5 more denials only counted, storage unchanged at {full} bytes; largest record +{biggest} bytes <= 500 reserved; underfunded init panics BALANCE_BELOW_CAP_AND_STORAGE"),
        Some(&approved),
    );
    Ok(())
}

/// The owner closes the locked mandate: the remainder goes only to `refund_to`, held money stays
/// payable, and the agent is refused from then on.
async fn close(w: &mut World<impl near_workspaces::Network + 'static>) -> Result<()> {
    let vendor = w.vendor.id().clone();
    let held: Value = w.spend("keep-1", &vendor, 4).await?.json()?;
    ensure!(held["status"] == "AWAITING_APPROVAL", "{held}");
    let account = w.worker.view_account(w.mandate.id()).await?;
    let treasury_start = w.balance(w.treasury.id()).await?;
    let free = account.balance.as_yoctonear() - u128::from(account.storage_usage) * 10u128.pow(19) - w.n(4);
    let closed = w.owner.call(w.mandate.id(), "close").gas(Gas::from_tgas(100)).transact().await?;
    ensure!(closed.is_success() && closed.receipt_failures().is_empty(), "{closed:?}");
    let refund = w.balance(w.treasury.id()).await? - treasury_start;
    ensure!(refund == free, "refund_to got {refund}, free balance was {free}");
    let event = events(&closed).into_iter().find(|e| e["event"] == "closed").context("no closed event")?;
    ensure!(event["data"][0]["amount"] == refund.to_string() && event["data"][0]["refund_to"] == w.treasury.id().as_str(), "{event}");

    let vendor_start = w.balance(&vendor).await?;
    let approved = w.owner.call(w.mandate.id(), "approve").args_json(w.args("keep-1", &vendor, 4, 1)).gas(Gas::from_tgas(100)).transact().await?;
    ensure!(approved.is_success() && approved.receipt_failures().is_empty(), "{approved:?}");
    ensure!(w.balance(&vendor).await? - vendor_start == w.n(4), "held request not paid after close");
    let after = w.spend("after-close", &vendor, 1).await?;
    ensure!(failed_with(&after, "CLOSED") && w.request("after-close").await?.is_null(), "{after:?}");
    w.ev.held(
        "closing sends the remainder only to refund_to; held requests stay payable; the agent is refused",
        "contract",
        &format!("refund_to +{refund} yocto = balance - storage stake - held 4 steps; keep-1 approved after close and paid; next request panics CLOSED"),
        Some(&closed),
    );
    Ok(())
}

/// `res/preavis.wasm` was built from this source: its NEP-330 metadata names the commit it was
/// built from, and the contract's inputs haven't changed since. Byte identity needs Docker:
/// `npm run verify:reproducible`.
#[test]
fn prebuilt_is_from_this_source() -> Result<()> {
    let prebuilt = std::fs::read("res/preavis.wasm")?;
    let text = String::from_utf8_lossy(&prebuilt);
    let marker = "github.com/ianlyoo/preavis?rev=";
    let at = text.find(marker).context("res/preavis.wasm has no NEP-330 source revision")?;
    let rev = &text[at + marker.len()..at + marker.len() + 40];
    let inputs = ["src", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", "scripts/build-wasm.sh"];
    let diff = std::process::Command::new("git").args(["diff", "--quiet", rev, "--"]).args(inputs).status()?;
    ensure!(diff.success(), "the contract's inputs changed since {rev}, which res/preavis.wasm was built from; run with PREAVIS_WASM=source to test your changes");
    let hash = sha256_hex(&prebuilt);
    let recorded = std::fs::read_to_string("res/preavis.wasm.sha256")?;
    ensure!(recorded.split_whitespace().next() == Some(hash.as_str()), "res/preavis.wasm.sha256 says {recorded}, file is {hash}");
    println!("res/preavis.wasm (sha256 {hash}) was built from commit {rev}; the contract's inputs are unchanged since. Byte identity: npm run verify:reproducible (Docker)");
    Ok(())
}

/// The deployed code exposes no way to deploy code, add keys or edit the policy.
#[test]
fn no_admin_surface() -> Result<()> {
    let mut exports = exports(&contract_wasm()?)?;
    exports.sort();
    // contract_source_metadata is the NEP-330 view near-sdk adds to every contract.
    let expected = ["approve", "attempts", "close", "contract_source_metadata", "get_request", "mandate", "new", "on_transfer", "reject", "request_spend"];
    ensure!(exports == expected, "unexpected exported methods: {exports:?}");
    let source = std::fs::read_to_string("src/lib.rs")?;
    for action in ["deploy_contract", "add_full_access_key", "add_access_key", "delete_key", "delete_account", "create_account", "function_call", ".stake(", "env::storage_write", "env::promise_batch"] {
        ensure!(!source.contains(action), "src/lib.rs contains {action}");
    }
    ensure!(
        source.matches("Promise::new(").count() == 2
            && source.contains("Promise::new(attempt.request.payee.clone())\n            .transfer(")
            && source.contains("Promise::new(self.refund_to.clone()).transfer("),
        "outgoing actions changed"
    );
    println!("exported methods: {exports:?}; the only outgoing actions are a transfer to the request's payee and, on close, to refund_to");
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
