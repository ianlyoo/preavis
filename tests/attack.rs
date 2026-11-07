//! `npm run attack-local`: runs the attacks in attack/inputs.json (or $PREAVIS_ATTACKS) against a
//! fresh sandbox mandate with the agent's key, then checks that no invariant broke.
mod common;

use anyhow::{Context, Result, ensure};
use common::*;
use near_workspaces::types::{Gas, NearToken};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// "$near:1.5" → yoctoNEAR, exactly.
fn yocto(near: &str) -> Result<String> {
    let (whole, fraction) = near.split_once('.').unwrap_or((near, ""));
    ensure!(fraction.len() <= 24, "too many decimals in {near}");
    let value = whole.parse::<u128>()? * NEAR + format!("{fraction:0<24}").parse::<u128>()?;
    Ok(value.to_string())
}

fn substitute(value: &Value, names: &BTreeMap<&str, String>) -> Result<Value> {
    Ok(match value {
        Value::String(s) if s.starts_with("$near:") => Value::String(yocto(&s[6..])?),
        Value::String(s) => Value::String(names.get(s.as_str()).cloned().unwrap_or_else(|| s.clone())),
        Value::Array(items) => Value::Array(items.iter().map(|v| substitute(v, names)).collect::<Result<_>>()?),
        Value::Object(map) => Value::Object(
            map.iter().map(|(k, v)| Ok((k.clone(), substitute(v, names)?))).collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

/// The interesting part of a refusal, e.g. `MethodNameMismatch` or `OWNER_ONLY`.
fn short(error: &str) -> String {
    for (start, ends) in [("InvalidAccessKeyError(", &[" {", ")"][..]), ("panicked: ", &["\"", "\\n"][..])] {
        if let Some(at) = error.find(start) {
            let rest = &error[at + start.len()..];
            let end = ends.iter().filter_map(|e| rest.find(e)).min().unwrap_or(rest.len().min(80));
            return rest[..end].to_string();
        }
    }
    error.chars().take(120).collect()
}

#[tokio::test]
async fn attack_local() -> Result<()> {
    let path = std::env::var("PREAVIS_ATTACKS").unwrap_or_else(|_| "attack/inputs.json".into());
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).with_context(|| path.clone())?)
        .with_context(|| format!("{path} is not valid JSON"))?;
    let attacks = file["attacks"].as_array().context("inputs need an \"attacks\" array")?;

    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let mut w = setup(worker, root.clone(), NEAR / 10, NearToken::from_near(10), &contract_wasm()?, false).await?;
    w.hand_over_agent().await?;
    // Your own account pays gas at the sandbox price (0.1 NEAR per 100 Tgas prepaid).
    root.transfer_near(w.attacker.id(), NearToken::from_near(10)).await?.into_result()?;
    let names: BTreeMap<&str, String> = [
        ("$attacker", w.attacker.id()),
        ("$vendor", w.vendor.id()),
        ("$hosting", w.hosting.id()),
        ("$ghost", &w.ghost),
        ("$mandate", w.mandate.id()),
        ("$owner", w.owner.id()),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.to_string()))
    .collect();
    let payees = [w.vendor.id().clone(), w.hosting.id().clone()];
    let mut start = BTreeMap::new();
    for id in &payees {
        start.insert(id.clone(), w.balance(id).await?);
    }

    let mut received: BTreeMap<String, u128> = BTreeMap::new();
    let mut receipts_read = 0;
    println!("\n{:>3}  {:<8} {:<14} outcome", "#", "as", "method");
    for (n, attack) in attacks.iter().enumerate() {
        let signer = match attack["as"].as_str().unwrap_or("agent") {
            "agent" => &w.agent,
            "attacker" => &w.attacker,
            other => anyhow::bail!("attack {n}: \"as\" must be agent or attacker, not {other}"),
        };
        let method = attack["method"].as_str().context("each attack needs a method")?;
        let deposit: u128 = attack["deposit_yocto"].as_str().unwrap_or("0").parse()?;
        let args = substitute(&attack["args"], &names)?;
        let sent = signer
            .call(w.mandate.id(), method)
            .args_json(args)
            .deposit(NearToken::from_yoctonear(deposit))
            .gas(Gas::from_tgas(100))
            .transact()
            .await;
        if let Ok(result) = &sent {
            receipts_read += result.outcomes().iter().map(|o| o.receipt_ids.len()).sum::<usize>();
            for t in transfers(&w.worker, result).await? {
                let to = t["to"].as_str().context("to")?.to_string();
                *received.entry(to).or_default() += t["amount"].as_str().context("amount")?.parse::<u128>()?;
            }
        }
        let outcome = match sent {
            Err(error) => format!("refused by the protocol: {}", short(&format!("{error:?}"))),
            Ok(result) if result.is_failure() => format!("refused by the contract: {}", short(&format!("{:?}", result.failures()))),
            Ok(result) => match result.json::<Value>() {
                Ok(a) if a["status"] == "DENIED" => format!("denied, recorded: {}", a["reason"].as_str().unwrap_or("")),
                Ok(a) if a["status"] == "AWAITING_APPROVAL" => "held for the owner; reserved, not paid".into(),
                Ok(a) if a["status"] == "SETTLED" => "PAID (inside the mandate)".into(),
                Ok(a) => format!("{}", a["status"]),
                Err(_) => "succeeded with no attempt returned".into(),
            },
        };
        println!("{n:>3}  {:<8} {method:<14} {outcome}", attack["as"].as_str().unwrap_or("agent"));
        println!("     {}", attack["note"].as_str().unwrap_or(""));
    }

    print_attempts(&w.mandate).await?;
    let attempts: Vec<Value> = w.mandate.view("attempts").args_json(json!({"from": 0, "limit": 1000})).await?.json()?;
    let summary = w.summary().await?;
    let amount = |a: &Value| a["amount"].as_str().unwrap_or("0").parse::<u128>().unwrap_or(0);
    let moved = |payee: &str| -> u128 {
        attempts
            .iter()
            .filter(|a| a["payee"] == payee && (a["status"] == "SETTLED" || a["status"] == "PENDING"))
            .map(amount)
            .sum()
    };
    let committed: u128 = summary["committed"].as_str().context("committed")?.parse()?;
    let reserved: u128 = attempts
        .iter()
        .filter(|a| ["SETTLED", "PENDING", "AWAITING_APPROVAL"].contains(&a["status"].as_str().unwrap_or("")))
        .map(amount)
        .sum();
    let total_cap: u128 = summary["policy"]["total_cap"].as_str().context("total_cap")?.parse()?;
    let paid_you = received.get(w.attacker.id().as_str()).copied().unwrap_or(0);
    let mut broken = vec![];
    if paid_you > 0 {
        broken.push(format!("you received {paid_you} yoctoNEAR"));
    }
    for payee in &payees {
        let delta = w.balance(payee).await? - start[payee];
        let cap: u128 = summary["policy"]["payees"][payee.as_str()].as_str().context("cap")?.parse()?;
        let measured = received.get(payee.as_str()).copied().unwrap_or(0);
        if delta != moved(payee.as_str()) || measured != delta || delta > cap {
            broken.push(format!("{payee} balance +{delta}, transfers +{measured}, records say {}, cap {cap}", moved(payee.as_str())));
        }
    }
    if committed != reserved || committed > total_cap {
        broken.push(format!("committed {committed}, records reserve {reserved}, total cap {total_cap}"));
    }
    if attempts.iter().any(|a| a["releases"].as_u64().unwrap_or(0) > 1) {
        broken.push("a charge was released twice".into());
    }
    if w.worker.view_account(&w.ghost).await.is_ok() {
        broken.push("the ghost payee exists".into());
    }

    println!();
    println!("You made the agent pay you: {paid_you} yoctoNEAR.");
    println!("  (measured: the transfers to {} in all {receipts_read} receipts of your attacks, read back from the node)", w.attacker.id());
    if broken.is_empty() {
        println!("Invariants held: payees received exactly what settled records say (by balance and by transfer receipts)");
        println!("and never more than their caps;");
        println!("committed {committed} <= total cap {total_cap}; no charge released twice.");
    }
    ensure!(broken.is_empty(), "INVARIANT BROKEN: {broken:?}");
    Ok(())
}
