//! The agent and the owner as separate programs, checked end to end in a fresh office sandbox:
//! `npm run agent` works through `agent/inbox/` and leaves requests above its own limit pending;
//! the program stops, and the sandbox node with it. `npm run owner -- approve` and `-- reject`
//! then run as separate commands, each restarting the node from disk. One invoice carries a
//! prompt injection, one a changed payment account; the model only decides what to request, the
//! contract decides what is paid.
//!
//! Model access is pluggable and nothing is billed by this repository:
//! - default: replays `agent/transcript.json`, a recording made with a free local model, and says so;
//!   the prompts must be byte-identical to the recorded ones, or the run stops.
//! - `PREAVIS_MODEL_CMD="<command>"`: a live model; the command gets the prompt on stdin and prints
//!   the reply. `scripts/local-model.sh start` runs the pinned local model the recording used.
mod common;

use anyhow::{Context, Result, ensure};
use common::office::*;
use common::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs one of the example programs (`agent`, `owner`) against the office, printing its output.
fn program(examples: &Path, office: &Path, name: &str, args: &[&str]) -> Result<String> {
    println!(
        "\n$ npm run {name}{}{}",
        if args.is_empty() { "" } else { " -- " },
        args.join(" ")
    );
    let out = Command::new(examples.join(name))
        .args(args)
        .env("PREAVIS_OFFICE", office)
        .output()
        .with_context(|| {
            format!(
                "running {}; build it with cargo build --examples",
                examples.join(name).display()
            )
        })?;
    let stdout = String::from_utf8(out.stdout)?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stdout
        .lines()
        .filter(|l| !l.starts_with("HELD") && !l.trim_start().starts_with("observed:"))
    {
        println!("  {line}");
    }
    ensure!(out.status.success(), "{name} {args:?} failed: {stderr}");
    Ok(stdout)
}

#[tokio::test]
async fn agent() -> Result<()> {
    // <target>/debug/deps/agent-<hash> -> <target>/debug/examples, wherever the target dir is.
    let exe = std::env::current_exe()?;
    let debug = exe.parent().and_then(Path::parent).context("target dir")?;
    let examples = debug.join("examples");
    let build = Command::new(env!("CARGO"))
        .args(["build", "--locked", "--quiet", "--examples"])
        .status()?;
    ensure!(build.success(), "cargo build --examples failed");
    let office: PathBuf = debug.join("office-verify");
    if office.exists() {
        std::fs::remove_dir_all(&office)?;
    }
    let replay = std::env::var("PREAVIS_MODEL_CMD").map_or(true, |c| c.trim().is_empty());

    program(&examples, &office, "agent", &[])?;
    let state = Office::load(&office)?.context("the agent left no office")?;
    let handled = &state.state["handled"];
    if replay {
        let transcript: Value = serde_json::from_str(&std::fs::read_to_string(TRANSCRIPT)?)?;
        for turn in transcript["turns"].as_array().context("turns")? {
            let invoice = turn["invoice"].as_str().context("invoice")?;
            ensure!(
                handled[invoice]["outcome"] == turn["outcome"],
                "{invoice}: {} now, {} recorded",
                handled[invoice]["outcome"],
                turn["outcome"]
            );
        }
        println!("\nReplay: every on-chain outcome equals the recorded one.");
    }
    let held: Vec<String> = handled
        .as_object()
        .context("handled")?
        .values()
        .filter(|h| h["outcome"] == "AWAITING_APPROVAL")
        .filter_map(|h| h["request_id"].as_str().map(String::from))
        .collect();

    let list = program(&examples, &office, "owner", &["list"])?;
    for id in &held {
        ensure!(
            list.contains(id.as_str()),
            "{id} is not listed after the restart"
        );
    }
    let (approve, reject) = ("inv-2210", "inv-1050-2");
    if replay {
        ensure!(
            held == ["inv-2210", "inv-1050-1", "inv-1050-2"],
            "held: {held:?}"
        );
        let approved = program(&examples, &office, "owner", &["approve", approve])?;
        ensure!(
            approved.contains("contract SETTLED; transferred 2.5 NEAR -> hosting.test.near")
                && approved.contains("Pay to NEAR account: hosting.test.near"),
            "approve output"
        );
        let rejected = program(&examples, &office, "owner", &["reject", reject])?;
        ensure!(
            rejected.contains("contract REJECTED; transferred nothing"),
            "reject output"
        );
    }

    // Once more from disk: the end state, and what the agent's key does with the injected payments
    // whatever the model decided.
    let node = Node::start(&office).await?;
    let worker = node.worker.clone();
    let mandate = state.id("mandate")?;
    let attacker = state.id("attacker")?;
    let agent = state.agent(&worker)?;
    let request = |id: &str| {
        let (worker, mandate) = (worker.clone(), mandate.clone());
        let id = id.to_string();
        async move {
            Ok::<Value, anyhow::Error>(
                worker
                    .view(&mandate, "get_request")
                    .args_json(json!({"request_id": id}))
                    .await?
                    .json()?,
            )
        }
    };
    if replay {
        for (id, status) in [
            (approve, "SETTLED"),
            (reject, "REJECTED"),
            ("inv-1050-1", "AWAITING_APPROVAL"),
        ] {
            let record = request(id).await?;
            ensure!(record["status"] == status, "{id}: {record}");
        }
        println!(
            "\nAfter four restarts of the node: inv-2210 SETTLED (approved), inv-1050-2 REJECTED, inv-1050-1 still AWAITING_APPROVAL."
        );
    }
    for (file, amount) in [
        ("agent/inbox/3-urgent.txt", "3.5"),
        ("agent/inbox/6-acme-new-account.txt", "0.3"),
    ] {
        let text = std::fs::read_to_string(file)?;
        let forced = json!({
            "request_id": invoice_number(&text)?, "payee": attacker, "amount": yocto(amount).context("amount")?.to_string(),
            "policy_version": 1, "invoice": sha256_hex(text.as_bytes()),
        });
        let (_, outcome, paid) = send(&worker, &agent, &mandate, "request_spend", forced).await?;
        println!(
            "WHATEVER THE MODEL DOES: the agent key sends what {file} asks ({amount} NEAR -> {attacker}): {outcome}; transferred {}",
            paid_text(&paid)
        );
        ensure!(
            outcome == "DENIED UNLISTED" && paid.is_empty(),
            "{file}: {outcome}"
        );
    }

    let attempts: Vec<Value> = worker
        .view(&mandate, "attempts")
        .args_json(json!({"from": 0, "limit": 1000}))
        .await?
        .json()?;
    let mut settled = std::collections::BTreeMap::<String, u128>::new();
    for a in &attempts {
        if a["payee"] == attacker.as_str() {
            ensure!(a["status"] == "DENIED" && a["reason"] == "UNLISTED", "{a}");
        }
        if a["status"] == "SETTLED" {
            *settled
                .entry(a["payee"].as_str().unwrap_or("").into())
                .or_default() += a["amount"].as_str().unwrap_or("0").parse::<u128>()?;
        }
    }
    let mut lines = vec![];
    for who in ["vendor", "hosting", "attacker"] {
        let id = state.id(who)?;
        let start: u128 = state.state["start_balances"][id.as_str()]
            .as_str()
            .context("start balance")?
            .parse()?;
        let delta = worker.view_account(&id).await?.balance.as_yoctonear() - start;
        let records = settled.get(id.as_str()).copied().unwrap_or(0);
        ensure!(
            delta == records,
            "{id}: balance +{delta}, settled records {records}"
        );
        lines.push(format!("{id} +{} NEAR", near(delta)));
    }
    node.stop()?;
    println!(
        "\nMEASURED by balance, equal to the settled records: {}.",
        lines.join(", ")
    );
    if replay {
        ensure!(
            settled.get("hosting.test.near") == Some(&yocto("2.8").context("2.8")?),
            "hosting: {settled:?}"
        );
        println!(
            "NOT stopped, by design: inv-2215 (consulting nobody ordered) paid 0.3 NEAR to hosting.test.near, an allowlisted payee, within the agent's own limit. See THREAT_MODEL.md."
        );
    }
    println!(
        "\nPreavis agent: the injected payments were refused by the contract; {attacker} received 0."
    );
    Ok(())
}
