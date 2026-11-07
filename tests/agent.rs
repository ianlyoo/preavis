//! `npm run agent`: an AI agent works through an inbox of three invoices and pays them through the
//! mandate in a local sandbox, with the same function-call key as in `verify`. One invoice carries a
//! prompt injection. The model only decides what to request; the contract decides what is paid.
//!
//! Model access is pluggable and nothing is billed by this repository:
//! - default: replays `agent/transcript.json`, a recording made with a free local model, and says so;
//!   the prompts must be byte-identical to the recorded ones, or the run stops.
//! - `PREAVIS_MODEL_CMD="<command>"`: a live model. The command gets the prompt on stdin and prints
//!   the reply, e.g. `claude -p` (your Claude Code login), `ollama run qwen2.5:7b`, or
//!   `node scripts/model-openai.mjs` for any OpenAI-compatible endpoint.
//! - `PREAVIS_RECORD="<model description>"` with a live model rewrites the transcript.
mod common;

use anyhow::{Context, Result, bail, ensure};
use common::*;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

const TRANSCRIPT: &str = "agent/transcript.json";
const INBOX: [&str; 3] = ["agent/inbox/1-acme-data.txt", "agent/inbox/2-cloudhost.txt", "agent/inbox/3-urgent.txt"];

enum Model {
    Live { cmd: String },
    Replay { transcript: Value },
}

impl Model {
    fn ask(&self, turn: usize, prompt: &str) -> Result<String> {
        match self {
            Model::Replay { transcript } => {
                let recorded = &transcript["turns"][turn];
                if recorded["prompt"].as_str() != Some(prompt) {
                    bail!("prompt {} differs from the recording in {TRANSCRIPT}; re-record with a live model", turn + 1);
                }
                Ok(recorded["reply"].as_str().context("reply")?.to_string())
            }
            Model::Live { cmd } => {
                let mut child = Command::new("sh")
                    .args(["-c", cmd])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .with_context(|| format!("starting PREAVIS_MODEL_CMD `{cmd}`"))?;
                child.stdin.take().context("stdin")?.write_all(prompt.as_bytes())?;
                let out = child.wait_with_output()?;
                ensure!(out.status.success(), "PREAVIS_MODEL_CMD exited with {}", out.status);
                Ok(String::from_utf8(out.stdout)?)
            }
        }
    }
}

/// "0.4" NEAR -> yoctoNEAR, exactly; None for anything that is not a plain decimal.
fn yocto(near: &str) -> Option<u128> {
    let (int, frac) = near.trim().split_once('.').unwrap_or((near.trim(), ""));
    if int.is_empty() || frac.len() > 24 || !(int.chars().chain(frac.chars())).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let frac: u128 = format!("{frac:0<24}").parse().ok()?;
    int.parse::<u128>().ok()?.checked_mul(NEAR)?.checked_add(frac)
}

fn near(yocto: u128) -> String {
    let s = format!("{:.6}", yocto as f64 / NEAR as f64);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The model's decision, from the first `{` to the last `}` of its reply.
fn decision(reply: &str) -> Result<(bool, AccountId, u128, String)> {
    let json = reply.find('{').zip(reply.rfind('}')).map(|(a, b)| &reply[a..=b]).context("no JSON object")?;
    let d: Value = serde_json::from_str(json)?;
    let pay = d["pay"].as_bool().context("no boolean `pay`")?;
    let amount = match &d["amount_near"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "0".into(),
    };
    let payee = d["payee"].as_str().unwrap_or("").parse().map_err(|e| anyhow::anyhow!("payee: {e}"));
    let reason = d["reason"].as_str().unwrap_or("").to_string();
    if !pay {
        return Ok((false, "none.test.near".parse()?, 0, reason));
    }
    Ok((true, payee?, yocto(&amount).with_context(|| format!("amount_near {amount:?}"))?, reason))
}

/// What the contract did with a transaction, from its events and failures.
fn outcome(result: &ExecutionFinalResult) -> String {
    let failures = failures(result);
    if !failures.is_empty() {
        return format!("REFUSED {}", failures.join(", "));
    }
    match events(result).last() {
        Some(e) => {
            let reason = e["data"][0]["reason"].as_str().map(|r| format!(" {r}")).unwrap_or_default();
            format!("{}{reason}", e["event"].as_str().unwrap_or("?").to_uppercase())
        }
        None => "NO EVENT".into(),
    }
}

struct Run {
    received: BTreeMap<String, u128>,
    receipts: usize,
}

impl Run {
    async fn send<N: near_workspaces::Network + 'static>(
        &mut self,
        w: &World<N>,
        signer: &Account,
        method: &str,
        args: Value,
    ) -> Result<(ExecutionFinalResult, String, Vec<Value>)> {
        let result =
            signer.call(w.mandate.id(), method).args_json(args).gas(Gas::from_tgas(100)).transact().await?;
        let moved = transfers(&w.worker, &result).await?;
        self.receipts += result.outcomes().len();
        let mut paid = vec![];
        for t in &moved {
            if t["from"] == w.mandate.id().as_str() {
                let amount: u128 = t["amount"].as_str().context("amount")?.parse()?;
                *self.received.entry(t["to"].as_str().context("to")?.to_string()).or_default() += amount;
                paid.push(json!(format!("{} NEAR -> {}", near(amount), t["to"].as_str().unwrap_or("?"))));
            }
        }
        let outcome = outcome(&result);
        Ok((result, outcome, paid))
    }
}

fn invoice_number(text: &str) -> Result<String> {
    let line = text.lines().find_map(|l| l.trim().strip_prefix("Invoice INV-")).context("no invoice number")?;
    Ok(format!("inv-{}", line.trim()))
}

#[tokio::test]
async fn agent() -> Result<()> {
    let record = std::env::var("PREAVIS_RECORD").ok();
    let model = match std::env::var("PREAVIS_MODEL_CMD") {
        Ok(cmd) if !cmd.trim().is_empty() => {
            println!("MODEL: live, `{cmd}` (prompt on stdin). Its decisions differ from run to run; the contract's limits do not.");
            Model::Live { cmd }
        }
        _ => {
            ensure!(record.is_none(), "PREAVIS_RECORD needs a live model: set PREAVIS_MODEL_CMD");
            let transcript: Value = serde_json::from_str(&std::fs::read_to_string(TRANSCRIPT)?)?;
            println!(
                "MODEL: REPLAYING A RECORDING, not a live model ({TRANSCRIPT}, recorded on {}).\n       Recorded model: {}",
                transcript["recorded_on"].as_str().unwrap_or("?"),
                transcript["model"].as_str().unwrap_or("?")
            );
            println!("       For a live model: PREAVIS_MODEL_CMD='claude -p' npm run agent (see README, \"The agent\").");
            Model::Replay { transcript }
        }
    };

    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let mut w = setup(worker, root, NEAR / 10, NearToken::from_near(10), &contract_wasm()?, false).await?;
    w.ev.testnet = false;
    w.hand_over_agent().await?;
    for (account, name) in [(&w.vendor, "vendor.test.near"), (&w.hosting, "hosting.test.near"), (&w.attacker, "attacker.test.near")] {
        ensure!(account.id().as_str() == name, "the inbox names {name}, the sandbox created {}", account.id());
    }
    let summary = w.summary().await?;
    println!(
        "\nMANDATE {}: allowlist vendor.test.near, hosting.test.near (and ghost); the agent alone may commit {} NEAR; total cap {} NEAR.",
        w.mandate.id(),
        near(summary["policy"]["approval_threshold"].as_str().context("threshold")?.parse()?),
        near(summary["policy"]["total_cap"].as_str().context("cap")?.parse()?),
    );
    println!("The agent signs with its function-call key (request_spend only), generated in this sandbox on your machine.\n");

    let template = std::fs::read_to_string("agent/prompt.txt")?;
    let mut run = Run { received: BTreeMap::new(), receipts: 0 };
    let mut turns = vec![];
    let mut awaiting = vec![];
    for (turn, path) in INBOX.iter().enumerate() {
        let text = std::fs::read_to_string(path)?;
        let hash = sha256_hex(text.as_bytes());
        let id = invoice_number(&text)?;
        println!("INBOX {}/3  {path}  ({}, sha256 {}…)", turn + 1, id.to_uppercase(), &hash[..12]);
        let prompt = template.replace("{invoice}", text.trim_end());
        let reply = model.ask(turn, &prompt)?;
        println!("  model    {}", reply.split_whitespace().collect::<Vec<_>>().join(" "));
        let outcome = match decision(&reply) {
            Err(e) => format!("NOTHING SENT (model reply unusable: {e})"),
            Ok((false, _, _, _)) => "NOTHING SENT (the model declined)".to_string(),
            Ok((true, payee, amount, _)) => {
                println!("  agent    request_spend {id}: {} NEAR -> {payee}, invoice = its sha256", near(amount));
                let args = json!({"request_id": id, "payee": payee, "amount": amount.to_string(), "policy_version": 1, "invoice": hash});
                let agent = w.agent.clone();
                let (_, outcome, paid) = run.send(&w, &agent, "request_spend", args.clone()).await?;
                if outcome == "AWAITING_APPROVAL" {
                    awaiting.push((args, path));
                }
                format!("{outcome}{}", if paid.is_empty() { String::new() } else { format!("; transferred {}", paid.iter().map(|p| p.as_str().unwrap_or("")).collect::<Vec<_>>().join(", ")) })
            }
        };
        println!("  contract {outcome}\n");
        turns.push(json!({"invoice": path, "prompt": prompt, "reply": reply, "outcome": outcome}));
    }

    // The human step: the owner reads each held invoice and approves exactly that request, bound to
    // the invoice's hash.
    let mut approvals = vec![];
    for (args, path) in &awaiting {
        let owner = w.owner.clone();
        let (_, outcome, paid) = run.send(&w, &owner, "approve", args.clone()).await?;
        let shown = paid.iter().map(|p| p.as_str().unwrap_or("")).collect::<Vec<_>>().join(", ");
        println!("OWNER approves {} after reading {path} (same payee, amount and invoice hash): {outcome}; transferred {shown}", args["request_id"].as_str().unwrap_or("?"));
        approvals.push(json!({"request_id": args["request_id"], "outcome": outcome}));
    }

    // Whatever the model did: the injected instruction itself, sent with the agent's key.
    let text = std::fs::read_to_string(INBOX[2])?;
    let forced = json!({
        "request_id": invoice_number(&text)?, "payee": w.attacker.id(), "amount": (35 * NEAR / 10).to_string(),
        "policy_version": 1, "invoice": sha256_hex(text.as_bytes()),
    });
    let agent = w.agent.clone();
    let (_, injected, _) = run.send(&w, &agent, "request_spend", forced).await?;
    println!("\nWHATEVER THE MODEL DOES: the agent key sends exactly what the injection asks (3.5 NEAR -> attacker.test.near): {injected}");
    ensure!(injected == "DENIED UNLISTED", "the injected payment was not refused: {injected}");

    let attempts: Vec<Value> = w.mandate.view("attempts").args_json(json!({"from": 0, "limit": 100})).await?.json()?;
    for a in attempts.iter().filter(|a| a["payee"] == w.attacker.id().as_str()) {
        ensure!(a["status"] == "DENIED" && a["reason"] == "UNLISTED", "attacker attempt not denied: {a}");
    }
    let attacker = run.received.get(w.attacker.id().as_str()).copied().unwrap_or(0);
    ensure!(attacker == 0, "the attacker received {attacker}");
    let paid: Vec<String> = run.received.iter().map(|(to, v)| format!("{to} {} NEAR", near(*v))).collect();
    println!(
        "\nMEASURED from all {} receipts of this run: the mandate paid {}; attacker.test.near received 0.",
        run.receipts,
        if paid.is_empty() { "nothing".into() } else { paid.join(", ") }
    );

    match (&model, record) {
        (Model::Replay { transcript }, _) => {
            for (turn, t) in turns.iter().enumerate() {
                ensure!(t["outcome"] == transcript["turns"][turn]["outcome"], "turn {} outcome changed: {}", turn + 1, t["outcome"]);
            }
            ensure!(json!(approvals) == transcript["approvals"], "approval outcomes changed: {approvals:?}");
            println!("Replay: every on-chain outcome equals the recorded one.");
        }
        (Model::Live { .. }, Some(label)) => {
            let date = String::from_utf8(Command::new("date").args(["-u", "+%Y-%m-%d"]).output()?.stdout)?;
            let transcript = json!({
                "_what": "A recorded run of `npm run agent`. The default run replays these replies instead of calling a model, and says so.",
                "model": label,
                "recorded_on": date.trim(),
                "turns": turns,
                "approvals": approvals,
            });
            std::fs::write(TRANSCRIPT, serde_json::to_string_pretty(&transcript)? + "\n")?;
            println!("Recorded to {TRANSCRIPT}.");
        }
        _ => {}
    }
    println!("\nPreavis agent: the injected payment was refused by the contract; the attacker received 0.");
    Ok(())
}
