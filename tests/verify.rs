//! `npm run verify`: a fresh local sandbox, a freshly generated agent key, every claim asserted
//! against the committed `res/preavis.wasm` (or a native build of `src/` with PREAVIS_WASM=source).
mod common;

use anyhow::{Context, Result, ensure};
use common::*;
use near_workspaces::AccountId;
use near_workspaces::types::{Gas, NearToken};
use serde_json::{Value, json};

#[tokio::test]
async fn verify() -> Result<()> {
    let wasm = contract_wasm()?;
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let mut w = setup(
        worker,
        root,
        NEAR / 10,
        NearToken::from_near(10),
        &wasm,
        false,
    )
    .await?;
    main_flow(&mut w).await?;
    callback_replay(&mut w).await?;
    w.lock_and_spend().await?;
    overlapping_requests(&mut w, &wasm).await?;
    callback_failure(&mut w).await?;
    bounded_storage(&mut w, &wasm).await?;
    periods(&mut w, &wasm).await?;
    expiry(&mut w, &wasm).await?;
    close(&mut w).await?;
    w.fire_agent().await?;

    print_attempts(&w.mandate).await?;
    println!(
        "\nPreavis verify: {} claims held, 0 broken.",
        w.ev.rows.len()
    );
    Ok(())
}

/// Runs before the lock: only the deployer's key can make the mandate call its own callback.
async fn callback_replay(w: &mut World<impl near_workspaces::Network + 'static>) -> Result<()> {
    let record = w.request("ghost-1").await?;
    let committed = w.committed().await?;
    for _ in 0..2 {
        let replay = w
            .mandate
            .call("on_transfer")
            .args_json(json!({"index": record["index"]}))
            .transact()
            .await?;
        ensure!(replay.is_success(), "{replay:?}");
        ensure!(
            w.request("ghost-1").await? == record && w.committed().await? == committed,
            "callback replay changed state"
        );
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
async fn overlapping_requests(
    w: &mut World<impl near_workspaces::Network + 'static>,
    wasm: &[u8],
) -> Result<()> {
    let agent = subaccount(&w.owner, "agent2", NearToken::from_near(1)).await?;
    let policy = json!({
        "version": 1, "payees": {w.vendor.id().as_str(): w.n(30).to_string()},
        "total_cap": w.n(6).to_string(), "approval_threshold": w.n(6).to_string(),
        "max_payments": 10, "max_denials": 10, "period_blocks": PERIOD_BLOCKS, "expires_at": w.expires_at,
    });
    let (mandate, _) = deploy_mandate(
        &w.owner,
        "mandate2",
        NearToken::from_near(10),
        wasm,
        agent.id(),
        w.treasury.id(),
        policy.clone(),
    )
    .await?;

    let again = mandate
        .as_account()
        .call(mandate.id(), "new")
        .args_json(json!({"owner": agent.id(), "agent": w.attacker.id(), "refund_to": w.attacker.id(), "policy": policy}))
        .transact()
        .await?;
    ensure!(failed_with(&again, "already been initialized"), "{again:?}");
    let summary: Value = mandate.view("mandate").await?.json()?;
    ensure!(
        summary["owner"] == w.owner.id().as_str()
            && summary["refund_to"] == w.treasury.id().as_str(),
        "{summary}"
    );
    let stranger = w
        .attacker
        .call(mandate.id(), "new")
        .args_json(json!({"owner": w.attacker.id(), "agent": agent.id(), "refund_to": w.attacker.id(), "policy": summary["policy"]}))
        .transact()
        .await?;
    ensure!(failed_with(&stranger, "is private"), "{stranger:?}");
    w.ev.held(
        "the mandate can't be initialized twice: owner, agent, refund_to and policy stay as set",
        "contract",
        "new() again from the mandate's own key: panic \"The contract has already been initialized\"; from any other account: panic \"Method new is private\"",
        Some(&again),
    );

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
    ensure!(
        w.balance(w.vendor.id()).await? - vendor_start == w.n(4),
        "both overlapping requests paid"
    );

    let denied_at = w
        .height(results[denied].receipt_outcomes()[0].block_hash)
        .await?;
    let callback = results[paid]
        .receipt_outcomes()
        .iter()
        .find(|o| o.logs.iter().any(|l| l.contains("\"event\":\"settled\"")))
        .context("no settlement callback")?;
    let settled_at = w.height(callback.block_hash).await?;
    ensure!(
        denied_at < settled_at,
        "requests did not overlap: denied at {denied_at}, settled at {settled_at}"
    );
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
    let policy = policy(
        w.vendor.id(),
        w.hosting.id(),
        &w.ghost,
        w.step,
        w.expires_at,
    );
    let (mandate, _) = deploy_mandate(
        &w.owner,
        "mandate3",
        NearToken::from_near(10),
        &broken,
        agent.id(),
        w.treasury.id(),
        policy,
    )
    .await?;
    let vendor_start = w.balance(w.vendor.id()).await?;
    let request = |id: &str| {
        agent
            .call(mandate.id(), "request_spend")
            .args_json(w.args(id, w.vendor.id(), 3, 1))
            .gas(Gas::from_tgas(100))
            .transact()
    };
    let first = request("frozen-1").await?;
    ensure!(
        format!("{:?}", first.receipt_failures()).contains("INJECTED_CALLBACK_FAILURE"),
        "{first:?}"
    );
    let view = |method: &'static str, args: Value| {
        let mandate = mandate.clone();
        async move { Ok::<Value, anyhow::Error>(mandate.view(method).args_json(args).await?.json()?) }
    };
    let record = view("get_request", json!({"request_id": "frozen-1"})).await?;
    ensure!(
        record["status"] == "PENDING" && record["releases"] == 0,
        "{record}"
    );
    ensure!(view("mandate", json!({})).await?["committed"] == w.n(3).to_string());
    let retry: Value = request("frozen-1").await?.json()?;
    ensure!(
        retry["status"] == "DENIED" && retry["reason"] == "REPLAY",
        "{retry}"
    );
    ensure!(
        w.balance(w.vendor.id()).await? - vendor_start == w.n(3),
        "paid more than once"
    );
    ensure!(view("mandate", json!({})).await?["committed"] == w.n(3).to_string());
    w.ev.held(
        "if the settlement callback fails, the charge stays frozen and visible; no refund, no resend",
        "settlement",
        "test-only build (feature inject-callback-failure): vendor +3 steps once, record PENDING, committed 3, retry DENIED REPLAY",
        Some(&first),
    );
    Ok(())
}

/// Hostile requests can't grow storage without bound: per period a mandate takes at most
/// `max_payments` requests and stores at most `max_denials` denials, and init refuses a balance
/// that doesn't cover the total cap plus storage for all of them in every period up to expiry.
async fn bounded_storage(
    w: &mut World<impl near_workspaces::Network + 'static>,
    wasm: &[u8],
) -> Result<()> {
    let agent = subaccount(&w.owner, "agent4", NearToken::from_near(1)).await?;
    let policy = |total: u128| {
        json!({
            "version": 1, "payees": {w.vendor.id().as_str(): w.n(30).to_string(), w.ghost.as_str(): w.n(10).to_string()},
            "total_cap": total.to_string(), "approval_threshold": w.n(10).to_string(), "max_payments": 3, "max_denials": 3,
            "period_blocks": PERIOD_BLOCKS, "expires_at": w.expires_at,
        })
    };
    let poor = subaccount(&w.owner, "poor", NearToken::from_near(3))
        .await?
        .deploy(wasm)
        .await?
        .into_result()?;
    let refused = poor.call("new").args_json(json!({"owner": w.owner.id(), "agent": agent.id(), "refund_to": w.treasury.id(), "policy": policy(NEAR)})).transact().await?;
    ensure!(
        failed_with(&refused, "BALANCE_BELOW_CAP_AND_STORAGE"),
        "{refused:?}"
    );

    let (mandate, _) = deploy_mandate(
        &w.owner,
        "mandate4",
        NearToken::from_near(10),
        wasm,
        agent.id(),
        w.treasury.id(),
        policy(w.n(40)),
    )
    .await?;
    let storage = || async {
        Ok::<u64, anyhow::Error>(w.worker.view_account(mandate.id()).await?.storage_usage)
    };
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
        ("p4", vendor.clone(), 1, "EXHAUSTED"),
        ("d1", w.attacker.id().clone(), 1, "UNLISTED"),
        ("d2", w.attacker.id().clone(), 1, "UNLISTED"),
    ];
    for (tag, payee, steps, outcome) in expected {
        let before = storage().await?;
        let mut attempt = send(long(tag), payee, steps).await?;
        if attempt["status"] == "PENDING" {
            attempt = mandate
                .view("get_request")
                .args_json(json!({"request_id": long(tag)}))
                .await?
                .json()?;
        }
        ensure!(
            attempt["status"] == outcome || attempt["reason"] == outcome,
            "{tag}: {attempt}"
        );
        ensure!(!attempt["index"].is_null(), "{tag} not stored");
        biggest = biggest.max(storage().await? - before);
    }
    ensure!(
        biggest <= 500,
        "one record grew storage by {biggest} bytes, more than the 500 the reserve assumes"
    );
    let full = storage().await?;
    for n in 0..5 {
        let attempt = send(long(&format!("extra{n}")), w.attacker.id().clone(), 1).await?;
        ensure!(
            attempt["reason"] == "UNLISTED" && attempt["index"].is_null(),
            "{attempt}"
        );
    }
    ensure!(storage().await? == full, "storage grew past the limits");
    let approved = w.owner.call(mandate.id(), "approve")
        .args_json(json!({"request_id": long("p3"), "payee": vendor, "amount": w.n(20).to_string(), "policy_version": 1, "invoice": "f".repeat(64)}))
        .gas(Gas::from_tgas(100)).transact().await?;
    ensure!(
        approved.is_success() && approved.receipt_failures().is_empty(),
        "{approved:?}"
    );
    let summary: Value = mandate.view("mandate").await?.json()?;
    ensure!(
        summary["payments"] == 3
            && summary["denials"] == 3
            && summary["denials_not_stored"] == 5
            && summary["exhausted"] == true
            && summary["resets_at"].as_u64() > Some(w.worker.view_block().await?.height()),
        "{summary}"
    );
    ensure!(storage().await? == full, "settling grew storage");
    w.ev.held(
        "hostile requests can't grow storage without bound, and the held request still settles",
        "contract",
        &format!("limits per period 3 payments + 3 stored denials; the 4th request DENIED EXHAUSTED, mandate() shows exhausted until resets_at {}; 5 more denials only counted, storage unchanged at {full} bytes; largest record +{biggest} bytes <= 500 reserved; underfunded init panics BALANCE_BELOW_CAP_AND_STORAGE", summary["resets_at"]),
        Some(&approved),
    );
    Ok(())
}

/// The agent's own budget and the request slots are per period. Requests on both sides of a
/// period's first block show the counters reset exactly there; a failed transfer frees its
/// period's budget; a held request is checked against the caps again when it is approved.
async fn periods(w: &mut World<near_workspaces::network::Sandbox>, wasm: &[u8]) -> Result<()> {
    const P: u64 = 40;
    let agent = subaccount(&w.owner, "agent5", NearToken::from_near(1)).await?;
    let start = w.worker.view_block().await?.height();
    let policy = json!({
        "version": 1,
        "payees": {w.vendor.id().as_str(): w.n(6).to_string(), w.hosting.id().as_str(): w.n(30).to_string(), w.ghost.as_str(): w.n(5).to_string()},
        "total_cap": w.n(40).to_string(), "approval_threshold": w.n(3).to_string(),
        "max_payments": 4, "max_denials": 2, "period_blocks": P, "expires_at": start + 50 * P,
    });
    let (mandate, _) = deploy_mandate(
        &w.owner,
        "mandate5",
        NearToken::from_near(10),
        wasm,
        agent.id(),
        w.treasury.id(),
        policy,
    )
    .await?;
    let summary = || async { Ok::<Value, anyhow::Error>(mandate.view("mandate").await?.json()?) };
    let step = w.step;
    let args = move |id: &str, payee: &AccountId, steps: u128| json!({"request_id": id, "payee": payee, "amount": (steps * step).to_string(), "policy_version": 1, "invoice": format!("invoice-{id}")});
    let send = |id: &str, payee: &AccountId, steps: u128| {
        agent
            .call(mandate.id(), "request_spend")
            .args_json(args(id, payee, steps))
            .gas(Gas::from_tgas(100))
            .transact()
    };
    let status =
        |result: &near_workspaces::result::ExecutionFinalResult| -> Result<(String, String)> {
            let attempt: Value = result.clone().json()?;
            Ok((
                attempt["status"].as_str().unwrap_or("").to_string(),
                attempt["reason"].as_str().unwrap_or("").to_string(),
            ))
        };
    let (vendor, hosting, ghost) = (
        w.vendor.id().clone(),
        w.hosting.id().clone(),
        w.ghost.clone(),
    );

    // Period 0: 3 steps of the agent's own budget and 4 request slots.
    let mut seen = vec![];
    for (id, payee, steps, expected) in [
        ("p0-a", &vendor, 2, "SETTLED"),
        ("p0-b", &vendor, 2, "AWAITING_APPROVAL"),
        ("p0-c", &vendor, 1, "SETTLED"),
        ("p0-d", &vendor, 1, "AWAITING_APPROVAL"),
    ] {
        let result = send(id, payee, steps).await?;
        let mut got = status(&result)?.0;
        if got == "PENDING" {
            got = mandate
                .view("get_request")
                .args_json(json!({"request_id": id}))
                .await?
                .json::<Value>()?["status"]
                .as_str()
                .unwrap_or("")
                .into();
        }
        ensure!(got == expected, "{id}: {got}, expected {expected}");
        seen.push(format!("{id} {got}"));
    }
    let s = summary().await?;
    ensure!(
        s["period"] == 0
            && s["period_committed"] == w.n(3).to_string()
            && s["period_payments"] == 4
            && s["exhausted"] == true,
        "{s}"
    );
    let resets_at = s["resets_at"].as_u64().context("resets_at")?;
    ensure!(
        resets_at == s["period_start"].as_u64().unwrap_or(0) + P,
        "{s}"
    );

    // Walk across the edge: every request is EXHAUSTED until the block `resets_at`, then the
    // first one in the new period settles from a fresh budget.
    let now = w.worker.view_block().await?.height();
    if resets_at > now + 16 {
        w.worker.fast_forward(resets_at - now - 16).await?;
    }
    let (mut before, mut after) = (0, None);
    for n in 0..40 {
        let id = format!("edge-{n}");
        let result = send(&id, &vendor, 1).await?;
        let landed = w.height(result.receipt_outcomes()[0].block_hash).await?;
        let (got, reason) = status(&result)?;
        if landed < resets_at {
            ensure!(
                got == "DENIED" && reason == "EXHAUSTED",
                "{id} at {landed} < {resets_at}: {got} {reason}"
            );
            before += 1;
        } else {
            let record = mandate
                .view("get_request")
                .args_json(json!({"request_id": id}))
                .await?
                .json::<Value>()?;
            ensure!(
                record["status"] == "SETTLED" && record["period"] == 1,
                "{id} at {landed} >= {resets_at}: {record}"
            );
            after = Some((id, landed, result));
            break;
        }
    }
    let (edge_id, landed, edge) = after.context("never crossed into period 1")?;
    ensure!(before > 0, "no request landed before the edge");
    let s = summary().await?;
    ensure!(
        s["period"] == 1
            && s["period_committed"] == w.n(1).to_string()
            && s["period_payments"] == 1
            && s["exhausted"] == false,
        "{s}"
    );
    w.ev.held(
        "the agent's own budget and request slots are per period: exhausted until the period's last block, fresh from the next one's first",
        "contract",
        &format!("period of {P} blocks, 3 steps and 4 requests each; period 0: {}; requests landing before block {resets_at}: {before}, all DENIED EXHAUSTED; {edge_id} landing at block {landed} SETTLED in period 1", seen.join(", ")),
        Some(&edge),
    );

    // Release inside the period: a failed transfer gives its share of the period budget back.
    let failed = send("p1-ghost", &ghost, 2).await?;
    ensure!(failed.receipt_failures().len() == 1, "{failed:?}");
    let s = summary().await?;
    ensure!(s["period_committed"] == w.n(1).to_string(), "{s}");
    let refill = send("p1-host", &hosting, 2).await?;
    let record = mandate
        .view("get_request")
        .args_json(json!({"request_id": "p1-host"}))
        .await?
        .json::<Value>()?;
    ensure!(
        record["status"] == "SETTLED" && record["needs_approval"] == false,
        "{record}"
    );
    w.ev.held(
        "a failed transfer gives its share of the period's budget back",
        "settlement",
        "period 1: 1 step used, a 2-step transfer to the never-created account FAILED, period budget back to 1 step; the next 2-step request settles without the owner",
        Some(&refill),
    );

    // Approval is checked against the caps as they are then: p0-b fits, p0-d no longer does.
    let owner = w.owner.clone();
    let approve = |id: &str, steps: u128| {
        owner
            .call(mandate.id(), "approve")
            .args_json(args(id, &vendor, steps))
            .gas(Gas::from_tgas(100))
            .transact()
    };
    let vendor_start = w.balance(&vendor).await?;
    let ok = approve("p0-b", 2).await?;
    ensure!(
        ok.is_success() && ok.receipt_failures().is_empty(),
        "{ok:?}"
    );
    let over = approve("p0-d", 1).await?;
    ensure!(failed_with(&over, "PayeeCap"), "{over:?}");
    ensure!(
        w.balance(&vendor).await? - vendor_start == w.n(2),
        "vendor paid more than p0-b"
    );
    let held = mandate
        .view("get_request")
        .args_json(json!({"request_id": "p0-d"}))
        .await?
        .json::<Value>()?;
    ensure!(held["status"] == "AWAITING_APPROVAL", "{held}");
    let s = summary().await?;
    ensure!(
        s["payee_committed"][vendor.as_str()] == w.n(6).to_string()
            && s["held"] == w.n(1).to_string(),
        "{s}"
    );
    w.ev.held(
        "a held request reserves nothing: its approval is checked against the caps as they are then",
        "contract",
        "vendor cap 6 steps, 4 settled; approving p0-b (2) brings vendor to 6 and pays; approving p0-d (1) panics PayeeCap, p0-d stays AWAITING_APPROVAL",
        Some(&over),
    );
    Ok(())
}

/// From `expires_at` on nothing is requested or approved, and anyone may close the mandate, which
/// sends everything but the storage stake to `refund_to`, fixed at init.
async fn expiry(w: &mut World<near_workspaces::network::Sandbox>, wasm: &[u8]) -> Result<()> {
    let agent = subaccount(&w.owner, "agent6", NearToken::from_near(1)).await?;
    let expires_at = w.worker.view_block().await?.height() + 40;
    let policy = json!({
        "version": 1, "payees": {w.vendor.id().as_str(): w.n(30).to_string()},
        "total_cap": w.n(10).to_string(), "approval_threshold": w.n(1).to_string(),
        "max_payments": 5, "max_denials": 5, "period_blocks": 10, "expires_at": expires_at,
    });
    let (mandate, _) = deploy_mandate(
        &w.owner,
        "mandate6",
        NearToken::from_near(10),
        wasm,
        agent.id(),
        w.treasury.id(),
        policy,
    )
    .await?;
    let vendor = w.vendor.id().clone();
    let step = w.step;
    let args = move |id: &str| json!({"request_id": id, "payee": vendor, "amount": (2 * step).to_string(), "policy_version": 1, "invoice": format!("invoice-{id}")});
    let send = |id: &str| {
        agent
            .call(mandate.id(), "request_spend")
            .args_json(args(id))
            .gas(Gas::from_tgas(100))
            .transact()
    };
    let held: Value = send("late-1").await?.json()?;
    ensure!(held["status"] == "AWAITING_APPROVAL", "{held}");
    let stranger = subaccount(&w.owner, "stranger", NearToken::from_near(1)).await?;
    let close = || {
        stranger
            .call(mandate.id(), "close")
            .gas(Gas::from_tgas(100))
            .transact()
    };
    let early = close().await?;
    ensure!(failed_with(&early, "OWNER_ONLY"), "{early:?}");

    let now = w.worker.view_block().await?.height();
    if expires_at > now {
        w.worker.fast_forward(expires_at - now + 2).await?;
    }
    let request = send("late-2").await?;
    ensure!(failed_with(&request, "EXPIRED"), "{request:?}");
    let approve = w
        .owner
        .call(mandate.id(), "approve")
        .args_json(args("late-1"))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    ensure!(failed_with(&approve, "EXPIRED"), "{approve:?}");

    let account = w.worker.view_account(mandate.id()).await?;
    let free = account.balance.as_yoctonear() - u128::from(account.storage_usage) * 10u128.pow(19);
    let (treasury_start, stranger_start) = (
        w.balance(w.treasury.id()).await?,
        w.balance(stranger.id()).await?,
    );
    let closed = close().await?;
    ensure!(
        closed.is_success() && closed.receipt_failures().is_empty(),
        "{closed:?}"
    );
    let refund = w.balance(w.treasury.id()).await? - treasury_start;
    ensure!(
        refund == free,
        "refund_to got {refund}, free balance was {free}"
    );
    ensure!(
        w.balance(stranger.id()).await? < stranger_start,
        "the caller gained from close"
    );
    let event = events(&closed)
        .into_iter()
        .find(|e| e["event"] == "closed")
        .context("no closed event")?;
    ensure!(
        event["data"][0]["expired"] == true && event["data"][0]["amount"] == refund.to_string(),
        "{event}"
    );
    w.ev.held(
        "after expires_at nothing is paid, and anyone can close the mandate, to refund_to only",
        "contract",
        &format!("past block {expires_at}: request and approve panic EXPIRED; before it a stranger's close panicked OWNER_ONLY, after it the stranger's close sent {refund} yocto (balance - storage stake) to refund_to and nothing to the stranger"),
        Some(&closed),
    );
    Ok(())
}

/// The owner closes the locked mandate: the remainder goes only to `refund_to`; from then on the
/// agent is refused and held requests can only be rejected.
async fn close(w: &mut World<impl near_workspaces::Network + 'static>) -> Result<()> {
    let vendor = w.vendor.id().clone();
    let held: Value = w.spend("keep-1", &vendor, 4).await?.json()?;
    ensure!(held["status"] == "AWAITING_APPROVAL", "{held}");
    let account = w.worker.view_account(w.mandate.id()).await?;
    let treasury_start = w.balance(w.treasury.id()).await?;
    let free = account.balance.as_yoctonear() - u128::from(account.storage_usage) * 10u128.pow(19);
    let closed = w
        .owner
        .call(w.mandate.id(), "close")
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    ensure!(
        closed.is_success() && closed.receipt_failures().is_empty(),
        "{closed:?}"
    );
    let refund = w.balance(w.treasury.id()).await? - treasury_start;
    ensure!(
        refund == free,
        "refund_to got {refund}, free balance was {free}"
    );
    let event = events(&closed)
        .into_iter()
        .find(|e| e["event"] == "closed")
        .context("no closed event")?;
    ensure!(
        event["data"][0]["amount"] == refund.to_string()
            && event["data"][0]["refund_to"] == w.treasury.id().as_str()
            && event["data"][0]["expired"] == false,
        "{event}"
    );

    let vendor_start = w.balance(&vendor).await?;
    let approved = w
        .owner
        .call(w.mandate.id(), "approve")
        .args_json(w.args("keep-1", &vendor, 4, 1))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    ensure!(failed_with(&approved, "CLOSED"), "{approved:?}");
    ensure!(
        w.balance(&vendor).await? == vendor_start,
        "held request paid after close"
    );
    let rejected = w
        .owner
        .call(w.mandate.id(), "reject")
        .args_json(json!({"request_id": "keep-1"}))
        .transact()
        .await?;
    ensure!(
        rejected.clone().json::<Value>()?["status"] == "REJECTED",
        "{rejected:?}"
    );
    let after = w.spend("after-close", &vendor, 1).await?;
    ensure!(
        failed_with(&after, "CLOSED") && w.request("after-close").await?.is_null(),
        "{after:?}"
    );
    w.ev.held(
        "closing sends the remainder only to refund_to; afterwards nothing is approved or requested",
        "contract",
        &format!("refund_to +{refund} yocto = balance - storage stake; approving the held keep-1 panics CLOSED, rejecting it works; next request panics CLOSED"),
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
    let at = text
        .find(marker)
        .context("res/preavis.wasm has no NEP-330 source revision")?;
    let rev = &text[at + marker.len()..at + marker.len() + 40];
    let inputs = [
        "src",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo",
        "scripts/build-wasm.sh",
    ];
    let diff = std::process::Command::new("git")
        .args(["diff", "--quiet", rev, "--"])
        .args(inputs)
        .status()?;
    ensure!(
        diff.success(),
        "the contract's inputs changed since {rev}, which res/preavis.wasm was built from; run with PREAVIS_WASM=source to test your changes"
    );
    let hash = sha256_hex(&prebuilt);
    let recorded = std::fs::read_to_string("res/preavis.wasm.sha256")?;
    ensure!(
        recorded.split_whitespace().next() == Some(hash.as_str()),
        "res/preavis.wasm.sha256 says {recorded}, file is {hash}"
    );
    println!(
        "res/preavis.wasm (sha256 {hash}) was built from commit {rev}; the contract's inputs are unchanged since. Byte identity: npm run verify:reproducible (Docker)"
    );
    Ok(())
}

/// The host functions the contract may import. Nothing that deploys code, adds or deletes keys,
/// creates or deletes accounts, stakes, or calls another contract: the only outgoing receipts it
/// can build are a batch with one transfer and the callback to itself (`promise_then`).
pub const ALLOWED_IMPORTS: [&str; 22] = [
    "account_balance",
    "attached_deposit",
    "block_index",
    "current_account_id",
    "input",
    "log_utf8",
    "panic_utf8",
    "predecessor_account_id",
    "promise_batch_action_transfer",
    "promise_batch_create",
    "promise_result",
    "promise_results_count",
    "promise_return",
    "promise_then",
    "read_register",
    "register_len",
    "storage_has_key",
    "storage_read",
    "storage_remove",
    "storage_usage",
    "storage_write",
    "value_return",
];

/// The deployed code exposes no way to deploy code, add keys or edit the policy, and imports no
/// host function that could: checked on the wasm bytes, not only on the source.
#[test]
fn no_admin_surface() -> Result<()> {
    let wasm = contract_wasm()?;
    let mut exports = section_names(&wasm, 7)?;
    exports.sort();
    // contract_source_metadata is the NEP-330 view near-sdk adds to every contract.
    let expected = [
        "approve",
        "attempts",
        "close",
        "contract_source_metadata",
        "get_request",
        "mandate",
        "new",
        "on_transfer",
        "reject",
        "request_spend",
    ];
    ensure!(
        exports == expected,
        "unexpected exported methods: {exports:?}"
    );
    let imports = section_names(&wasm, 2)?;
    let unexpected: Vec<_> = imports
        .iter()
        .filter(|name| {
            !name
                .strip_prefix("env.")
                .is_some_and(|f| ALLOWED_IMPORTS.contains(&f))
        })
        .collect();
    ensure!(
        unexpected.is_empty(),
        "the wasm imports host functions outside the allowlist: {unexpected:?}"
    );
    let source = std::fs::read_to_string("src/lib.rs")?;
    ensure!(
        source.matches("env::promise_batch_create(").count() == 2
            && source
                .matches("env::promise_batch_action_transfer(")
                .count()
                == 2
            && source.matches("env::promise_then(").count() == 1
            && source.contains("env::promise_batch_create(&attempt.request.payee)")
            && source.contains("env::promise_batch_create(&self.refund_to)")
            && source.contains("env::current_account_id(),\n            \"on_transfer\""),
        "outgoing actions changed"
    );
    println!(
        "exported methods: {exports:?}; {} imported host functions, all in the allowlist (no deploy, key, account, stake or cross-contract call); the only outgoing receipts: a transfer to the request's payee then on_transfer on itself, and on close a transfer to refund_to",
        imports.len()
    );
    Ok(())
}

/// Function names in a wasm section: exports (id 7) as `name`, imports (id 2) as `module.name`.
fn section_names(wasm: &[u8], section: u8) -> Result<Vec<String>> {
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
    fn name(bytes: &[u8], at: &mut usize) -> Result<String> {
        let len = leb(bytes, at);
        let name = String::from_utf8(bytes[*at..*at + len].to_vec())?;
        *at += len;
        Ok(name)
    }
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let size = leb(wasm, &mut at);
        if id != section {
            at += size;
            continue;
        }
        let mut names = vec![];
        for _ in 0..leb(wasm, &mut at) {
            let mut full = name(wasm, &mut at)?;
            if section == 2 {
                full = format!("{full}.{}", name(wasm, &mut at)?);
            }
            let kind = wasm[at];
            at += 1;
            match (section, kind) {
                (7, _) | (2, 0) => {
                    leb(wasm, &mut at);
                }
                _ => anyhow::bail!("import {full} is not a function (kind {kind})"),
            }
            if kind == 0 {
                names.push(full);
            }
        }
        return Ok(names);
    }
    anyhow::bail!("no section {section}")
}
