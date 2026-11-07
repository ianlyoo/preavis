//! Known limits of the deployed contract, pinned as they are (THREAT_MODEL.md, "Not stopped" 12).
//! These tests assert the current behavior on purpose, so that a change to it is noticed: if a
//! future contract version fixes a limit, its test fails and the documentation must change with it.
mod common;

use anyhow::{Context, Result, ensure};
use common::*;
use near_workspaces::types::{Gas, NearToken};
use serde_json::{Value, json};

/// `new` accepts any nonzero `period_blocks`. With `u64::MAX` the `mandate` view's reset height
/// (`period_start + period_blocks`) overflows, so the summary view panics. Owner configuration only,
/// fixed at init; the spending checks are unaffected: one in-policy payment settles and the next is
/// denied, nothing beyond the caps moves. The deployed mandates use 1,008,000-block periods.
#[tokio::test]
async fn extreme_period_breaks_the_summary_view_only() -> Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.root_account()?;
    let agent = subaccount(&owner, "agent", NearToken::from_near(5)).await?;
    let vendor = subaccount(&owner, "vendor", NearToken::from_near(1)).await?;
    let wasm = contract_wasm()?;
    let height = worker.view_block().await?.height();
    let policy = json!({
        "version": 1,
        "payees": { vendor.id().as_str(): "1" },
        "total_cap": "1",
        "approval_threshold": "1",
        "period_blocks": u64::MAX,
        "expires_at": height + 1000,
        "max_payments": 1,
        "max_denials": 1,
    });
    let (mandate, _) = deploy_mandate(
        &owner,
        "mandate",
        NearToken::from_near(10),
        &wasm,
        agent.id(),
        owner.id(),
        policy,
    )
    .await
    .context("KNOWN LIMIT CHANGED: new() no longer accepts period_blocks = u64::MAX; update THREAT_MODEL.md \"Not stopped\" 12 and this test")?;
    println!("KNOWN LIMIT: new(period_blocks = u64::MAX, expires_at = current + 1000) is accepted");

    let view = mandate.view("mandate").await;
    let error = match view {
        Ok(_) => anyhow::bail!(
            "KNOWN LIMIT CHANGED: the mandate view now works with period_blocks = u64::MAX; update THREAT_MODEL.md \"Not stopped\" 12 and this test"
        ),
        Err(e) => format!("{e:?}"),
    };
    ensure!(
        error.contains("attempt to add with overflow"),
        "the mandate view failed differently: {error}"
    );
    println!("KNOWN LIMIT: mandate() panics: attempt to add with overflow (the reset height)");

    let records: Vec<Value> = mandate
        .view("attempts")
        .args_json(json!({"from": 0, "limit": 10}))
        .await?
        .json()?;
    ensure!(records.is_empty(), "{records:?}");

    let start = vendor.view_account().await?.balance.as_yoctonear();
    let mut outcomes = vec![];
    for id in ["one", "two"] {
        let r = agent
            .call(mandate.id(), "request_spend")
            .args_json(json!({
                "request_id": id, "payee": vendor.id(), "amount": "1",
                "policy_version": 1, "invoice": "limits",
            }))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?;
        let a: Value = r.json()?;
        outcomes.push(format!(
            "{} {}",
            a["status"].as_str().unwrap_or("?"),
            a["reason"].as_str().unwrap_or("")
        ));
    }
    let gained = vendor.view_account().await?.balance.as_yoctonear() - start;
    ensure!(
        outcomes[0].trim() == "SETTLED" && outcomes[1].starts_with("DENIED") && gained == 1,
        "spending under the extreme period: {outcomes:?}, vendor +{gained} yocto"
    );
    let committed: Value = mandate
        .view("get_request")
        .args_json(json!({"request_id": "one"}))
        .await?
        .json()?;
    ensure!(committed["status"] == "SETTLED", "{committed}");
    println!(
        "KNOWN LIMIT: spending is unaffected: request one {}, request two {}, vendor +{gained} yocto (cap 1)",
        outcomes[0].trim(),
        outcomes[1].trim()
    );
    println!(
        "\nPreavis limits: period_blocks = u64::MAX is accepted and breaks only the mandate() view; no spend beyond the caps."
    );
    Ok(())
}
