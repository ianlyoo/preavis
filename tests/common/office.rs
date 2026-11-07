//! The local office behind `npm run agent` and `npm run owner`: a sandbox node whose chain state
//! lives on disk (`.preavis/`, or `PREAVIS_OFFICE`), so the agent and the owner are separate
//! commands, each starting and stopping the node, and a held request survives between them.
//!
//! The agent process signs only with the agent's function-call key; the owner process signs with
//! the sandbox's root key, which owns the mandate. Both keys exist only in this local sandbox.

use super::*;
use anyhow::bail;
use near_workspaces::network::{Sandbox, ValidatorKey};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub const TRANSCRIPT: &str = "agent/transcript.json";
pub const INBOX: &str = "agent/inbox";
pub const PROMPT: &str = "agent/prompt.txt";

pub fn office_dir() -> PathBuf {
    std::env::var("PREAVIS_OFFICE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".preavis"))
}

/// A sandbox node started from the office's data directory and stopped again by `stop`.
pub struct Node {
    child: Option<Child>,
    pub worker: Worker<Sandbox>,
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

impl Node {
    pub async fn start(dir: &Path) -> Result<Node> {
        let bin = near_sandbox::install().map_err(|e| anyhow::anyhow!("near-sandbox: {e}"))?;
        let data = dir.join("sandbox");
        if !data.join("genesis.json").exists() {
            std::fs::create_dir_all(&data)?;
            let init = Command::new(&bin)
                .arg("--home")
                .arg(&data)
                .args(["init", "--fast"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            ensure!(init.success(), "near-sandbox init failed");
            near_sandbox::config::set_sandbox_genesis(&data)
                .map_err(|e| anyhow::anyhow!("sandbox genesis: {e}"))?;
        }
        let (rpc, net) = (free_port()?, free_port()?);
        let log = std::fs::File::create(dir.join("node.log"))?;
        let child = Command::new(&bin)
            .arg("--home")
            .arg(&data)
            .args(["run", "--rpc-addr", &format!("127.0.0.1:{rpc}")])
            .args(["--network-addr", &format!("127.0.0.1:{net}")])
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("starting the sandbox node")?;
        let mut node = Node {
            child: Some(child),
            worker: near_workspaces::sandbox()
                .rpc_addr(&format!("http://127.0.0.1:{rpc}"))
                .validator_key(ValidatorKey::HomeDir(data))
                .await?,
        };
        // Wait until the node produces blocks, not only answers.
        let first = node.worker.view_block().await?.height();
        for _ in 0..100 {
            if node.worker.view_block().await?.height() > first {
                return Ok(node);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        node.kill();
        anyhow::bail!(
            "the sandbox node produces no blocks; see {}",
            dir.join("node.log").display()
        )
    }

    /// Stops the node; its chain state stays on disk for the next command.
    pub fn stop(mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            Command::new("kill").arg(child.id().to_string()).status()?;
            child.wait()?;
        }
        Ok(())
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.kill();
    }
}

/// What the office remembers between commands: who is who, and which invoices the agent handled.
/// The agent's working key is kept apart, in `agent-key` (mode 0600).
pub struct Office {
    pub dir: PathBuf,
    pub state: Value,
}

impl Office {
    pub fn load(dir: &Path) -> Result<Option<Office>> {
        let path = dir.join("office.json");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(Office {
            dir: dir.to_path_buf(),
            state: serde_json::from_str(&std::fs::read_to_string(path)?)?,
        }))
    }

    pub fn save(&self) -> Result<()> {
        std::fs::write(
            self.dir.join("office.json"),
            serde_json::to_string_pretty(&self.state)? + "\n",
        )?;
        Ok(())
    }

    pub fn id(&self, who: &str) -> Result<AccountId> {
        Ok(self.state[who]
            .as_str()
            .with_context(|| format!("office.json has no {who}"))?
            .parse()?)
    }

    pub fn agent(&self, worker: &Worker<Sandbox>) -> Result<Account> {
        let key: SecretKey = std::fs::read_to_string(self.dir.join("agent-key"))?
            .trim()
            .parse()?;
        Ok(Account::from_secret_key(self.id("agent")?, key, worker))
    }

    /// First run: a fresh mandate in the office's sandbox, the agent handed its function-call key,
    /// the mandate locked.
    pub async fn create(dir: &Path, worker: &Worker<Sandbox>) -> Result<Office> {
        let root = worker.root_account()?;
        let mut w = setup(
            worker.clone(),
            root,
            NEAR / 10,
            NearToken::from_near(10),
            &contract_wasm()?,
            false,
        )
        .await?;
        w.hand_over_agent().await?;
        w.lock().await?;
        let key_path = dir.join("agent-key");
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
        file.open(&key_path)?
            .write_all(w.agent.secret_key().to_string().as_bytes())?;
        let mut balances = serde_json::Map::new();
        for account in [&w.vendor, &w.hosting, &w.attacker, &w.treasury] {
            balances.insert(
                account.id().to_string(),
                json!(w.balance(account.id()).await?.to_string()),
            );
        }
        let office = Office {
            dir: dir.to_path_buf(),
            state: json!({
                "mandate": w.mandate.id(), "owner": w.owner.id(), "agent": w.agent.id(),
                "vendor": w.vendor.id(), "hosting": w.hosting.id(), "attacker": w.attacker.id(),
                "treasury": w.treasury.id(), "ghost": w.ghost, "start_balances": balances,
                "handled": {},
            }),
        };
        office.save()?;
        Ok(office)
    }
}

pub enum Model {
    Live { cmd: String },
    Replay { transcript: Value },
}

impl Model {
    /// A live model if `PREAVIS_MODEL_CMD` is set, otherwise the recording, announced as such.
    pub fn from_env() -> Result<Model> {
        match std::env::var("PREAVIS_MODEL_CMD") {
            Ok(cmd) if !cmd.trim().is_empty() => {
                println!(
                    "MODEL: live, `{cmd}` (prompt on stdin). Its decisions differ from run to run; the contract's limits do not."
                );
                Ok(Model::Live { cmd })
            }
            _ => {
                let transcript: Value =
                    serde_json::from_str(&std::fs::read_to_string(TRANSCRIPT)?)?;
                println!(
                    "MODEL: REPLAYING A RECORDING, not a live model ({TRANSCRIPT}, recorded on {}).\n       Recorded model: {}",
                    transcript["recorded_on"].as_str().unwrap_or("?"),
                    transcript["model"].as_str().unwrap_or("?")
                );
                println!(
                    "       For a live model: PREAVIS_MODEL_CMD='<command>' npm run agent (README, \"The agent\")."
                );
                Ok(Model::Replay { transcript })
            }
        }
    }

    pub fn ask(&self, invoice: &str, prompt: &str) -> Result<String> {
        match self {
            Model::Replay { transcript } => {
                let recorded = transcript["turns"]
                    .as_array()
                    .context("turns")?
                    .iter()
                    .find(|t| t["invoice"] == invoice)
                    .with_context(|| {
                        format!("{invoice} is not in {TRANSCRIPT}; re-record with a live model")
                    })?;
                if recorded["prompt"].as_str() != Some(prompt) {
                    bail!(
                        "the prompt for {invoice} differs from the recording in {TRANSCRIPT}; re-record with a live model"
                    );
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
                child
                    .stdin
                    .take()
                    .context("stdin")?
                    .write_all(prompt.as_bytes())?;
                let out = child.wait_with_output()?;
                ensure!(
                    out.status.success(),
                    "PREAVIS_MODEL_CMD exited with {}",
                    out.status
                );
                Ok(String::from_utf8(out.stdout)?)
            }
        }
    }
}

/// "0.4" NEAR -> yoctoNEAR, exactly; None for anything that is not a plain decimal.
pub fn yocto(near: &str) -> Option<u128> {
    let (int, frac) = near.trim().split_once('.').unwrap_or((near.trim(), ""));
    if int.is_empty()
        || frac.len() > 24
        || !(int.chars().chain(frac.chars())).all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let frac: u128 = format!("{frac:0<24}").parse().ok()?;
    int.parse::<u128>()
        .ok()?
        .checked_mul(NEAR)?
        .checked_add(frac)
}

pub fn near(yocto: u128) -> String {
    let s = format!("{:.6}", yocto as f64 / NEAR as f64);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The model's decision, from the first `{` to the last `}` of its reply.
pub fn decision(reply: &str) -> Result<(bool, AccountId, u128, String)> {
    let json = reply
        .find('{')
        .zip(reply.rfind('}'))
        .map(|(a, b)| &reply[a..=b])
        .context("no JSON object")?;
    let d: Value = serde_json::from_str(json)?;
    let pay = d["pay"].as_bool().context("no boolean `pay`")?;
    let amount = match &d["amount_near"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "0".into(),
    };
    let payee = d["payee"]
        .as_str()
        .unwrap_or("")
        .parse()
        .map_err(|e| anyhow::anyhow!("payee: {e}"));
    let reason = d["reason"].as_str().unwrap_or("").to_string();
    if !pay {
        return Ok((false, "none.test.near".parse()?, 0, reason));
    }
    Ok((
        true,
        payee?,
        yocto(&amount).with_context(|| format!("amount_near {amount:?}"))?,
        reason,
    ))
}

/// What the contract did with a transaction, from its events and failures.
pub fn outcome(result: &ExecutionFinalResult) -> String {
    let failures = failures(result);
    if !failures.is_empty() {
        return format!("REFUSED {}", failures.join(", "));
    }
    match events(result).last() {
        Some(e) => {
            let reason = e["data"][0]["reason"]
                .as_str()
                .map(|r| format!(" {r}"))
                .unwrap_or_default();
            format!(
                "{}{reason}",
                e["event"].as_str().unwrap_or("?").to_uppercase()
            )
        }
        None => "NO EVENT".into(),
    }
}

/// The request ID for an invoice: `inv-` and the number after "Invoice INV-".
pub fn invoice_number(text: &str) -> Result<String> {
    let line = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Invoice INV-"))
        .context("no invoice number")?;
    Ok(format!("inv-{}", line.trim().to_lowercase()))
}

/// The inbox files, in name order.
pub fn inbox() -> Result<Vec<String>> {
    let mut files: Vec<String> = std::fs::read_dir(INBOX)?
        .filter_map(|e| e.ok())
        .map(|e| format!("{INBOX}/{}", e.file_name().to_string_lossy()))
        .filter(|p| p.ends_with(".txt"))
        .collect();
    files.sort();
    Ok(files)
}

/// One transfer out of the mandate, from the transaction's receipts: payee, amount, and whether the
/// receipt carrying it succeeded. A failed one was attempted and moved nothing.
#[derive(Clone, Debug)]
pub struct Transfer {
    pub to: String,
    pub amount: u128,
    pub succeeded: bool,
}

/// Sends one call to the mandate and returns the outcome and the transfers it attempted, measured
/// from the receipts with each receipt's execution status.
pub async fn send(
    worker: &Worker<Sandbox>,
    signer: &Account,
    mandate: &AccountId,
    method: &str,
    args: Value,
) -> Result<(ExecutionFinalResult, String, Vec<Transfer>)> {
    let result = signer
        .call(mandate, method)
        .args_json(args)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    let mut sent = vec![];
    for t in transfers(worker, &result).await? {
        if t["from"] == mandate.as_str() {
            sent.push(Transfer {
                to: t["to"].as_str().context("to")?.to_string(),
                amount: t["amount"].as_str().context("amount")?.parse()?,
                succeeded: t["succeeded"].as_bool().context("succeeded")?,
            });
        }
    }
    Ok((result.clone(), outcome(&result), sent))
}

/// Only the transfers that succeeded: what was paid.
pub fn paid(sent: &[Transfer]) -> Vec<&Transfer> {
    sent.iter().filter(|t| t.succeeded).collect()
}

fn list(sent: &[&Transfer]) -> String {
    sent.iter()
        .map(|t| format!("{} NEAR -> {}", near(t.amount), t.to))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What was paid, for the summary table: "nothing", or the settled transfers; failed ones are
/// added as attempted, never as paid.
pub fn paid_text(sent: &[Transfer]) -> String {
    let (ok, failed): (Vec<&Transfer>, Vec<&Transfer>) = sent.iter().partition(|t| t.succeeded);
    let paid = if ok.is_empty() { "nothing".into() } else { list(&ok) };
    if failed.is_empty() {
        paid
    } else {
        format!("{paid} ({} attempted, failed)", list(&failed))
    }
}

/// The measured transfers after "contract <outcome>; ": settled, attempted-and-failed, or none.
pub fn transfer_text(sent: &[Transfer]) -> String {
    let (ok, failed): (Vec<&Transfer>, Vec<&Transfer>) = sent.iter().partition(|t| t.succeeded);
    let mut parts = vec![];
    if !ok.is_empty() || failed.is_empty() {
        parts.push(format!(
            "transferred {}",
            if ok.is_empty() { "nothing".into() } else { list(&ok) }
        ));
    }
    if !failed.is_empty() {
        parts.push(format!("transfer of {} attempted, failed", list(&failed)));
    }
    let text = parts.join("; ");
    if ok.is_empty() && !failed.is_empty() {
        format!("{text}: nothing paid")
    } else {
        text
    }
}

/// `npm run agent`: works through the inbox once. Requests above the agent's own limit stay
/// AWAITING_APPROVAL in the mandate for the owner; the agent never approves anything.
pub async fn run_agent(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let record = std::env::var("PREAVIS_RECORD").ok();
    let model = Model::from_env()?;
    ensure!(
        record.is_none() || matches!(model, Model::Live { .. }),
        "PREAVIS_RECORD needs a live model: set PREAVIS_MODEL_CMD"
    );
    let node = Node::start(dir).await?;
    let worker = node.worker.clone();
    let mut office = match Office::load(dir)? {
        Some(office) => {
            println!(
                "office: {} (mandate {}, state from the previous run)",
                dir.display(),
                office.state["mandate"].as_str().unwrap_or("?")
            );
            office
        }
        None => {
            println!("office: {} is new; creating the mandate", dir.display());
            Office::create(dir, &worker).await?
        }
    };
    ensure!(
        record.is_none()
            || office.state["handled"]
                .as_object()
                .is_some_and(|h| h.is_empty()),
        "recording needs a fresh office: PREAVIS_OFFICE=<new dir>"
    );
    let mandate = office.id("mandate")?;
    let agent = office.agent(&worker)?;
    let summary: Value = worker.view(&mandate, "mandate").await?.json()?;
    println!(
        "MANDATE {mandate}: allowlist {}; the agent alone may commit {} NEAR per period of {} blocks; total cap {} NEAR.",
        summary["policy"]["payees"]
            .as_object()
            .map(|p| p.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default(),
        near(
            summary["policy"]["approval_threshold"]
                .as_str()
                .context("threshold")?
                .parse()?
        ),
        summary["policy"]["period_blocks"],
        near(
            summary["policy"]["total_cap"]
                .as_str()
                .context("cap")?
                .parse()?
        ),
    );
    println!(
        "The agent signs with its function-call key (request_spend only), kept in {}.\n",
        dir.join("agent-key").display()
    );

    let template = std::fs::read_to_string(PROMPT)?;
    let mut turns = vec![];
    let files = inbox()?;
    for (n, path) in files.iter().enumerate() {
        if let Some(done) = office.state["handled"].get(path) {
            println!(
                "INBOX {}/{}  {path}: handled in an earlier run ({})",
                n + 1,
                files.len(),
                done["outcome"].as_str().unwrap_or("?")
            );
            continue;
        }
        let text = std::fs::read_to_string(path)?;
        let hash = sha256_hex(text.as_bytes());
        let id = invoice_number(&text)?;
        println!(
            "INBOX {}/{}  {path}  ({}, sha256 {}…)",
            n + 1,
            files.len(),
            id.to_uppercase(),
            &hash[..12]
        );
        let prompt = template.replace("{invoice}", text.trim_end());
        let reply = model.ask(path, &prompt)?;
        println!(
            "  model    {}",
            reply.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        let (proposed, outcome, sent) = match decision(&reply) {
            Err(e) => (
                format!("nothing (reply unusable: {e})"),
                "NOTHING SENT".to_string(),
                vec![],
            ),
            Ok((false, _, _, _)) => ("decline".into(), "NOTHING SENT".to_string(), vec![]),
            Ok((true, payee, amount, _)) => {
                println!(
                    "  agent    request_spend {id}: {} NEAR -> {payee}, invoice = its sha256",
                    near(amount)
                );
                let args = json!({"request_id": id, "payee": payee, "amount": amount.to_string(), "policy_version": 1, "invoice": hash});
                let (_, outcome, sent) =
                    send(&worker, &agent, &mandate, "request_spend", args).await?;
                (
                    format!("pay {} NEAR -> {payee}", near(amount)),
                    outcome,
                    sent,
                )
            }
        };
        println!("  contract {outcome}; {}", transfer_text(&sent));
        if outcome == "AWAITING_APPROVAL" {
            println!("  held for the owner: npm run owner -- show {id}");
        }
        println!();
        let entry = json!({"request_id": id, "proposed": proposed, "outcome": outcome, "paid": paid_text(&sent)});
        office.state["handled"][path] = entry.clone();
        office.save()?;
        turns.push(json!({"invoice": path, "prompt": prompt, "reply": reply, "outcome": outcome}));
    }

    println!("PROPOSED BY THE MODEL vs MEASURED TRANSFERS (from each transaction's receipts)");
    println!(
        "{:<34} {:<14} {:<34} {:<22} {}",
        "invoice", "request", "proposed", "contract", "transferred"
    );
    for path in &files {
        let h = &office.state["handled"][path];
        println!(
            "{:<34} {:<14} {:<34} {:<22} {}",
            path.trim_start_matches("agent/inbox/"),
            h["request_id"].as_str().unwrap_or(""),
            h["proposed"].as_str().unwrap_or(""),
            h["outcome"].as_str().unwrap_or(""),
            h["paid"].as_str().unwrap_or("")
        );
    }

    if let (Model::Live { .. }, Some(label)) = (&model, record) {
        let date = String::from_utf8(
            Command::new("date")
                .args(["-u", "+%Y-%m-%d"])
                .output()?
                .stdout,
        )?;
        let transcript = json!({
            "_what": "A recorded run of `npm run agent`. The default run replays these replies instead of calling a model, and says so.",
            "model": label,
            "recorded_on": date.trim(),
            "turns": turns,
        });
        std::fs::write(
            TRANSCRIPT,
            serde_json::to_string_pretty(&transcript)? + "\n",
        )?;
        println!("\nRecorded to {TRANSCRIPT}.");
    }
    node.stop()?;
    println!(
        "\nThe node is stopped; the chain state stays in {}. Held requests wait for the owner: npm run owner -- list",
        dir.display()
    );
    Ok(())
}

/// `npm run owner -- list | show <id> | approve <id> | reject <id>`. Shows the invoice the agent
/// bound the request to (its bytes and sha256), the payee and the amount, then signs.
pub async fn run_owner(dir: &Path, args: &[String]) -> Result<()> {
    let usage = "usage: npm run owner -- list | show <request id> | approve <request id> | reject <request id>";
    let (command, id) = match args {
        [c] if c == "list" => ("list", None),
        [c, id] if ["show", "approve", "reject"].contains(&c.as_str()) => {
            (c.as_str(), Some(id.clone()))
        }
        _ => bail!("{usage}"),
    };
    let office = Office::load(dir)?
        .with_context(|| format!("no office in {}: run npm run agent first", dir.display()))?;
    let node = Node::start(dir).await?;
    let worker = node.worker.clone();
    let mandate = office.id("mandate")?;
    let owner = worker.root_account()?;
    ensure!(
        owner.id() == &office.id("owner")?,
        "the sandbox root is not the mandate's owner"
    );

    let Some(id) = id else {
        let attempts: Vec<Value> = worker
            .view(&mandate, "attempts")
            .args_json(json!({"from": 0, "limit": 1000}))
            .await?
            .json()?;
        let held: Vec<&Value> = attempts
            .iter()
            .filter(|a| a["status"] == "AWAITING_APPROVAL")
            .collect();
        println!(
            "{} request(s) awaiting your approval in {mandate}:",
            held.len()
        );
        for a in held {
            println!(
                "  {:<14} {:>8} NEAR -> {:<20} invoice sha256 {}",
                a["request_id"].as_str().unwrap_or(""),
                near(a["amount"].as_str().unwrap_or("0").parse()?),
                a["payee"].as_str().unwrap_or(""),
                a["invoice"].as_str().unwrap_or("")
            );
        }
        println!("Read one before deciding: npm run owner -- show <request id>");
        return node.stop();
    };

    let record: Value = worker
        .view(&mandate, "get_request")
        .args_json(json!({"request_id": id}))
        .await?
        .json()?;
    ensure!(!record.is_null(), "no request {id} in {mandate}");
    let amount: u128 = record["amount"].as_str().context("amount")?.parse()?;
    let hash = record["invoice"].as_str().unwrap_or("");
    println!(
        "REQUEST {id} in {mandate}: status {}",
        record["status"].as_str().unwrap_or("?")
    );
    println!("  payee    {}", record["payee"].as_str().unwrap_or(""));
    println!("  amount   {} NEAR ({amount} yoctoNEAR)", near(amount));
    println!("  invoice  sha256 {hash}");
    let file = inbox()?.into_iter().find(|p| {
        std::fs::read(p)
            .map(|b| sha256_hex(&b) == hash)
            .unwrap_or(false)
    });
    match &file {
        Some(path) => {
            let bytes = std::fs::read(path)?;
            println!(
                "  the invoice with that sha256 is {path} ({} bytes), verbatim:",
                bytes.len()
            );
            println!("  ----------------------------------------------------------------");
            for line in String::from_utf8_lossy(&bytes).lines() {
                println!("  | {line}");
            }
            println!("  ----------------------------------------------------------------");
        }
        None => println!(
            "  NO FILE in {INBOX} has this sha256: you can't read what this request pays for."
        ),
    }

    match command {
        "show" => {}
        "approve" => {
            ensure!(
                record["status"] == "AWAITING_APPROVAL",
                "{id} is not awaiting approval"
            );
            ensure!(
                file.is_some(),
                "refusing to approve a request whose invoice you can't read"
            );
            println!(
                "SIGNING approve with exactly these fields (request_id, payee, amount, policy_version, invoice)…"
            );
            let args = json!({
                "request_id": id, "payee": record["payee"], "amount": record["amount"],
                "policy_version": record["policy_version"], "invoice": record["invoice"],
            });
            let (_, outcome, sent) = send(&worker, &owner, &mandate, "approve", args).await?;
            println!("  contract {outcome}; {}", transfer_text(&sent));
        }
        "reject" => {
            println!("SIGNING reject {id}…");
            let (_, outcome, sent) = send(
                &worker,
                &owner,
                &mandate,
                "reject",
                json!({"request_id": id}),
            )
            .await?;
            println!("  contract {outcome}; {}", transfer_text(&sent));
        }
        _ => unreachable!(),
    }
    node.stop()
}
