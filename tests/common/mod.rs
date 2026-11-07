//! Shared fixture and the mandate scenarios that run on both the local sandbox and testnet.
#![allow(dead_code)] // each test binary uses a different subset

pub mod office;

use anyhow::{Context, Result, ensure};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{AccessKey, AccessKeyPermission, Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, Contract, Network, Worker};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const NEAR: u128 = 10u128.pow(24);

/// Builds the contract with `scripts/build-wasm.sh`, the same command the README gives for a rebuild.
pub fn wasm(feature: Option<&str>) -> Result<Vec<u8>> {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut build = std::process::Command::new("bash");
    build.arg(format!("{root}/scripts/build-wasm.sh"));
    let target = match feature {
        Some(feature) => {
            let dir = format!("{root}/target/{feature}");
            build.args(["--features", feature, "--target-dir", &dir]);
            dir
        }
        None => format!("{root}/target"),
    };
    ensure!(
        build
            .status()
            .context("run scripts/build-wasm.sh")?
            .success(),
        "wasm build failed"
    );
    Ok(std::fs::read(format!(
        "{target}/wasm32-unknown-unknown/release/preavis.wasm"
    ))?)
}

/// The contract the scenarios deploy: the committed `res/preavis.wasm` (the bytes testnet runs), or
/// with PREAVIS_WASM=source a native build of `src/`, to try your own changes.
pub fn contract_wasm() -> Result<Vec<u8>> {
    if std::env::var("PREAVIS_WASM").as_deref() == Ok("source") {
        return wasm(None);
    }
    Ok(std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/res/preavis.wasm"
    ))?)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One line per claim that held, printed as it happens and kept for the testnet receipt file.
pub struct Evidence {
    pub rows: Vec<Value>,
    pub testnet: bool,
}

impl Evidence {
    pub fn held(
        &mut self,
        claim: &str,
        layer: &str,
        observed: &str,
        result: Option<&ExecutionFinalResult>,
    ) {
        let tx = result.map(|r| r.outcome().transaction_hash.to_string());
        println!("HELD {:>2}  [{layer}] {claim}", self.rows.len() + 1);
        println!("         observed: {observed}");
        if let (true, Some(tx)) = (self.testnet, &tx) {
            println!("         https://testnet.nearblocks.io/txns/{tx}");
        }
        let status = result.map(|r| if r.is_success() { "success" } else { "failure" });
        let signer = result.map(|r| r.outcome().executor_id.to_string());
        self.rows.push(json!({
            "claim": claim, "layer": layer, "observed": observed, "tx": tx, "signer": signer, "tx_status": status,
            "events": result.map(events), "failures": result.map(failures),
        }));
    }
}

/// Every NEP-297 event the transaction's receipts logged, in order.
pub fn events(result: &ExecutionFinalResult) -> Vec<Value> {
    result
        .logs()
        .iter()
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Every NEAR transfer the transaction's receipts carried, as `{from, to, amount, succeeded}`, read
/// back from the node receipt by receipt. Gas refunds (sent by `system`) are left out.
///
/// These are transfer *actions*, attempted ones included: `succeeded` is the execution status of
/// the receipt that carried the action (false if the receipt failed, e.g. `AccountDoesNotExist`, or
/// if its outcome is missing). A failed transfer moved no money; callers that report payments must
/// count only `succeeded` ones. The invariant oracle (tests/fuzz.rs) checks every attempted one.
pub async fn transfers<N: Network + 'static>(
    worker: &Worker<N>,
    result: &ExecutionFinalResult,
) -> Result<Vec<Value>> {
    let status: std::collections::HashMap<String, bool> = result
        .outcomes()
        .iter()
        .map(|o| (o.transaction_hash.to_string(), o.is_success()))
        .collect();
    let mut out = vec![];
    for id in result.outcomes().iter().flat_map(|o| o.receipt_ids.clone()) {
        let succeeded = status.get(&id.to_string()).copied().unwrap_or(false);
        let receipt_id = id.to_string().parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        let receipt = worker
            .receipt(near_jsonrpc_primitives::types::receipts::ReceiptReference { receipt_id })
            .await?;
        let receipt = serde_json::to_value(&receipt)?;
        if receipt["predecessor_id"] == "system" {
            continue;
        }
        for action in receipt["receipt"]["Action"]["actions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(amount) = action["Transfer"].get("deposit") {
                out.push(json!({"from": receipt["predecessor_id"], "to": receipt["receiver_id"], "amount": amount, "succeeded": succeeded}));
            }
        }
    }
    Ok(out)
}

/// The signed transaction's receiver and actions, as the node reports them, for the receipt file.
pub async fn transaction<N: Network + 'static>(
    worker: &Worker<N>,
    tx: &str,
    signer: &str,
) -> Result<Value> {
    let info = near_jsonrpc_primitives::types::transactions::TransactionInfo::TransactionId {
        tx_hash: tx.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        sender_account_id: signer.parse()?,
    };
    let status = worker
        .tx_status(info, near_primitives::views::TxExecutionStatus::Final)
        .await?;
    let outcome = status
        .final_execution_outcome
        .context("no outcome")?
        .into_outcome();
    Ok(json!({"receiver": outcome.transaction.receiver_id, "actions": outcome.transaction.actions}))
}

/// Each failed receipt, reduced to its panic message or `AccountDoesNotExist`; the observer
/// reduces the chain's JSON the same way.
pub fn failures(result: &ExecutionFinalResult) -> Vec<String> {
    let mut out: Vec<String> = result
        .failures()
        .iter()
        .map(|f| {
            let text = format!("{f:?}");
            match text.split_once("panicked: ") {
                Some((_, rest)) => rest.split(['"', '\\']).next().unwrap_or("").to_string(),
                None if text.contains("AccountDoesNotExist") => "AccountDoesNotExist".into(),
                None => "other".into(),
            }
        })
        .collect();
    out.sort();
    out
}

pub struct World<N: Network + 'static> {
    pub worker: Worker<N>,
    pub owner: Account,
    pub agent: Account,
    pub mandate: Contract,
    pub vendor: Account,
    pub hosting: Account,
    /// An unlisted account standing in for you: the agent must never pay it.
    pub attacker: Account,
    /// `refund_to`, fixed at init: the only account `close` can send the remainder to.
    pub treasury: Account,
    /// Allowlisted but never created, so a transfer to it fails.
    pub ghost: AccountId,
    /// Every amount is a multiple of this; 0.1 NEAR in the sandbox, 0.0001 NEAR on testnet.
    pub step: u128,
    /// The agent's full-access key, held by the owner after the hand-over.
    pub agent_offline_key: Option<SecretKey>,
    /// The demo mandate's `expires_at` block height.
    pub expires_at: u64,
    pub ev: Evidence,
}

/// Always with a fresh random key: without `.keys()`, near-workspaces derives the key from a
/// fixed public seed, which on testnet would let anyone sign for the account.
pub async fn subaccount(owner: &Account, name: &str, balance: NearToken) -> Result<Account> {
    Ok(owner
        .create_subaccount(name)
        .keys(SecretKey::from_random(KeyType::ED25519))
        .initial_balance(balance)
        .transact()
        .await?
        .into_result()?)
}

/// The demo mandate's period: a week of blocks at testnet's pace, about 0.6 s a block.
pub const PERIOD_BLOCKS: u64 = 1_008_000;
/// The demo mandate expires this many periods after it is created.
pub const PERIODS: u64 = 6;

/// The fixed policy every demo mandate uses, in steps: vendor ≤ 30, hosting ≤ 30, ghost ≤ 10,
/// total ≤ 40; per period of `PERIOD_BLOCKS` the agent alone may commit 10, and at most 15
/// requests pass the checks and 15 denials are stored.
pub fn policy(
    vendor: &AccountId,
    hosting: &AccountId,
    ghost: &AccountId,
    step: u128,
    expires_at: u64,
) -> Value {
    json!({
        "max_payments": 15,
        "max_denials": 15,
        "period_blocks": PERIOD_BLOCKS,
        "expires_at": expires_at,
        "version": 1,
        "payees": {
            vendor.as_str(): (30 * step).to_string(),
            hosting.as_str(): (30 * step).to_string(),
            ghost.as_str(): (10 * step).to_string(),
        },
        "total_cap": (40 * step).to_string(),
        "approval_threshold": (10 * step).to_string(),
    })
}

pub async fn deploy_mandate(
    owner: &Account,
    name: &str,
    balance: NearToken,
    wasm: &[u8],
    agent: &AccountId,
    refund_to: &AccountId,
    policy: Value,
) -> Result<(Contract, ExecutionFinalResult)> {
    let account = subaccount(owner, name, balance).await?;
    let mandate = account.deploy(wasm).await?.into_result()?;
    let init = mandate
        .call("new")
        .args_json(
            json!({"owner": owner.id(), "agent": agent, "refund_to": refund_to, "policy": policy}),
        )
        .transact()
        .await?;
    ensure!(init.is_success(), "init failed: {init:?}");
    Ok((mandate, init))
}

pub async fn setup<N: Network + 'static>(
    worker: Worker<N>,
    owner: Account,
    step: u128,
    mandate_balance: NearToken,
    wasm: &[u8],
    testnet: bool,
) -> Result<World<N>> {
    let small = NearToken::from_millinear(50);
    let agent = subaccount(&owner, "agent", NearToken::from_millinear(500)).await?;
    let vendor = subaccount(&owner, "vendor", small).await?;
    let hosting = subaccount(&owner, "hosting", small).await?;
    let attacker = subaccount(&owner, "attacker", small).await?;
    let treasury = subaccount(&owner, "treasury", small).await?;
    let ghost: AccountId = format!("ghost.{}", owner.id()).parse()?;
    let expires_at = worker.view_block().await?.height() + PERIODS * PERIOD_BLOCKS;
    let policy = policy(vendor.id(), hosting.id(), &ghost, step, expires_at);
    let (mandate, init) = deploy_mandate(
        &owner,
        "mandate",
        mandate_balance,
        wasm,
        agent.id(),
        treasury.id(),
        policy,
    )
    .await?;
    let mut ev = Evidence {
        rows: vec![],
        testnet,
    };
    ev.held(
        "mandate deployed and initialized with a fixed policy",
        "setup",
        &format!("{} code sha256 {}", mandate.id(), sha256_hex(wasm)),
        Some(&init),
    );
    Ok(World {
        worker,
        owner,
        agent,
        mandate,
        vendor,
        hosting,
        attacker,
        treasury,
        ghost,
        step,
        agent_offline_key: None,
        expires_at,
        ev,
    })
}

impl<N: Network + 'static> World<N> {
    pub fn n(&self, steps: u128) -> u128 {
        steps * self.step
    }

    /// Request arguments; every test request names its invoice `invoice-<request id>`.
    pub fn args(&self, id: &str, payee: &AccountId, steps: u128, version: u32) -> Value {
        json!({
            "request_id": id, "payee": payee, "amount": self.n(steps).to_string(),
            "policy_version": version, "invoice": format!("invoice-{id}"),
        })
    }

    pub async fn spend_as(
        &self,
        signer: &Account,
        id: &str,
        payee: &AccountId,
        steps: u128,
        version: u32,
    ) -> Result<ExecutionFinalResult> {
        Ok(signer
            .call(self.mandate.id(), "request_spend")
            .args_json(self.args(id, payee, steps, version))
            .gas(Gas::from_tgas(100))
            .transact()
            .await?)
    }

    pub async fn spend(
        &self,
        id: &str,
        payee: &AccountId,
        steps: u128,
    ) -> Result<ExecutionFinalResult> {
        self.spend_as(&self.agent, id, payee, steps, 1).await
    }

    pub async fn summary(&self) -> Result<Value> {
        Ok(self.mandate.view("mandate").await?.json()?)
    }

    pub async fn committed(&self) -> Result<u128> {
        Ok(self.summary().await?["committed"]
            .as_str()
            .context("committed")?
            .parse()?)
    }

    pub async fn request(&self, id: &str) -> Result<Value> {
        Ok(self
            .mandate
            .view("get_request")
            .args_json(json!({"request_id": id}))
            .await?
            .json()?)
    }

    pub async fn attempt(&self, index: &Value) -> Result<Value> {
        let page: Vec<Value> = self
            .mandate
            .view("attempts")
            .args_json(json!({"from": index, "limit": 1}))
            .await?
            .json()?;
        page.into_iter().next().context("attempt not stored")
    }

    pub async fn height(&self, block: near_workspaces::CryptoHash) -> Result<u64> {
        Ok(self.worker.view_block().block_hash(block).await?.height())
    }

    pub async fn balance(&self, id: &AccountId) -> Result<u128> {
        Ok(self.worker.view_account(id).await?.balance.as_yoctonear())
    }

    /// Asserts a normal-returning denial: record committed, NEP-297 event logged, nothing charged.
    pub async fn expect_denied(
        &mut self,
        claim: &str,
        request: (u32, &str, &AccountId, u128),
        reason: &str,
    ) -> Result<()> {
        let (version, id, payee, steps) = request;
        let before = self.committed().await?;
        let result = self
            .spend_as(&self.agent, id, payee, steps, version)
            .await?;
        ensure!(
            result.is_success() && result.receipt_failures().is_empty(),
            "{claim}: a denial must return normally, got {result:?}"
        );
        let logged = result.logs().iter().any(|l| {
            l.starts_with("EVENT_JSON:") && l.contains("\"event\":\"denied\"") && l.contains(id)
        });
        let returned: Value = result.clone().json()?;
        let stored = self.attempt(&returned["index"]).await?;
        ensure!(
            returned == stored && stored["status"] == "DENIED" && stored["reason"] == reason,
            "{claim}: returned {returned}, stored {stored}"
        );
        ensure!(logged, "{claim}: no denied event");
        ensure!(self.committed().await? == before, "{claim}: budget changed");
        self.ev.held(
            claim,
            "contract",
            &format!(
                "DENIED {reason}, record #{} + event, budget unchanged",
                stored["index"]
            ),
            Some(&result),
        );
        Ok(())
    }

    /// Generates the agent's working key locally: a function-call key for `request_spend` only.
    /// The agent's full-access key leaves the agent and stays with the owner, offline; its one use
    /// is firing the agent by deleting the working key.
    pub async fn hand_over_agent(&mut self) -> Result<()> {
        let key = SecretKey::from_random(KeyType::ED25519);
        let changed = self
            .agent
            .batch(self.agent.id())
            .add_key(
                key.public_key(),
                AccessKey::function_call_access(
                    self.mandate.id(),
                    &["request_spend"],
                    Some(NearToken::from_millinear(250)),
                ),
            )
            .transact()
            .await?;
        ensure!(changed.is_success(), "key change failed: {changed:?}");
        let offline = self.agent.secret_key().clone();
        self.agent.set_secret_key(key.clone());
        let keys = self.agent.view_access_keys().await?;
        ensure!(keys.len() == 2, "agent has {} keys", keys.len());
        for k in &keys {
            match &k.access_key.permission {
                AccessKeyPermission::FunctionCall(p) => ensure!(
                    k.public_key == key.public_key()
                        && p.receiver_id == self.mandate.id().as_str()
                        && p.method_names == ["request_spend"],
                    "unexpected function-call key {p:?}"
                ),
                AccessKeyPermission::FullAccess => {
                    ensure!(
                        k.public_key == offline.public_key(),
                        "unknown full-access key"
                    )
                }
            }
        }
        println!();
        println!("  the agent: {}", self.agent.id());
        println!(
            "  its working key, public half: {} (the secret half was generated on this machine and signs every agent call below)",
            key.public_key()
        );
        println!(
            "  it can call {}.request_spend and nothing else.",
            self.mandate.id()
        );
        println!(
            "  the agent's full-access key is the owner's, offline; it only exists to fire the agent."
        );
        println!();
        self.ev.held(
            "the agent's working key (generated on this machine) is a function-call key for request_spend on the mandate only",
            "protocol",
            "agent keys: FunctionCall receiver=mandate methods=[request_spend] (the working key) + the owner's offline full-access key",
            Some(&changed),
        );
        self.agent_offline_key = Some(offline);
        Ok(())
    }

    /// The lock: deletes the mandate account's only key, the deployer's full-access key. Afterwards
    /// nobody can sign for the mandate account, so only its code can move its balance. Then shows
    /// the contract still works.
    pub async fn lock_and_spend(&mut self) -> Result<()> {
        self.lock().await?;
        let vendor = self.vendor.id().clone();
        let before = self.balance(&vendor).await?;
        let spent = self.spend("after-lock", &vendor, 1).await?;
        ensure!(
            spent.is_success() && spent.receipt_failures().is_empty(),
            "{spent:?}"
        );
        ensure!(self.request("after-lock").await?["status"] == "SETTLED");
        ensure!(
            self.balance(&vendor).await? - before == self.n(1),
            "vendor not paid exactly 1 step"
        );
        self.ev.held(
            "after the lock the contract still works: an in-policy request settles",
            "settlement",
            "SETTLED; vendor balance +1 step exactly",
            Some(&spent),
        );
        Ok(())
    }

    /// Deletes the mandate account's only key.
    pub async fn lock(&mut self) -> Result<()> {
        let account = self.mandate.as_account();
        let locked = account
            .batch(self.mandate.id())
            .delete_key(account.secret_key().public_key())
            .transact()
            .await?;
        ensure!(locked.is_success(), "{locked:?}");
        let keys = self.worker.view_access_keys(self.mandate.id()).await?;
        ensure!(keys.is_empty(), "mandate still has {} keys", keys.len());
        self.ev.held(
            "locked: the mandate account has no access key, so nobody can redeploy it or move its balance outside the contract",
            "protocol",
            "deleted the deployer's full-access key; view_access_key_list: 0 keys",
            Some(&locked),
        );
        Ok(())
    }

    /// The owner deletes the agent's working key with the offline full-access key.
    pub async fn fire_agent(&mut self) -> Result<()> {
        let offline = self
            .agent_offline_key
            .clone()
            .context("agent not handed over")?;
        let owner_side = Account::from_secret_key(self.agent.id().clone(), offline, &self.worker);
        let fired = owner_side
            .batch(self.agent.id())
            .delete_key(self.agent.secret_key().public_key())
            .transact()
            .await?;
        ensure!(fired.is_success(), "{fired:?}");
        let after = self
            .spend("after-firing", &self.vendor.id().clone(), 1)
            .await;
        let error = format!(
            "{:?}",
            after.err().context("a fired agent's key still works")?
        );
        ensure!(error.contains("InvalidAccessKeyError"), "{error}");
        ensure!(self.request("after-firing").await?.is_null());
        self.ev.held(
            "firing the agent takes one key deletion; its next request is refused",
            "protocol",
            &format!(
                "owner deleted the working key; next request: RPC error {}",
                if error.contains("AccessKeyNotFound") {
                    "InvalidAccessKeyError(AccessKeyNotFound)"
                } else {
                    "InvalidAccessKeyError"
                }
            ),
            Some(&fired),
        );
        Ok(())
    }
}

pub fn failed_with(result: &ExecutionFinalResult, message: &str) -> bool {
    result.is_failure() && format!("{:?}", result.failures()).contains(message)
}

/// The mandate scenarios. Every claim asserts on contract state, balances or execution outcomes.
pub async fn main_flow<N: Network + 'static>(w: &mut World<N>) -> Result<()> {
    let attacker_start = w.balance(w.attacker.id()).await?;
    let vendor_start = w.balance(w.vendor.id()).await?;
    let hosting_start = w.balance(w.hosting.id()).await?;
    let held_args = |w: &World<N>, steps: u128| w.args("split-b", w.vendor.id(), steps, 1);

    // Before the hand-over the agent still has a full-access key, so this call reaches the
    // contract and shows the contract's own guard, separate from the key restriction below.
    let probe = w
        .agent
        .call(w.mandate.id(), "reject")
        .args_json(json!({"request_id": "x"}))
        .transact()
        .await?;
    ensure!(
        failed_with(&probe, "OWNER_ONLY"),
        "agent reached an owner method: {probe:?}"
    );
    w.ev.held(
        "an owner method refuses the agent, even with a full-access key",
        "contract",
        "panic OWNER_ONLY, no state change",
        Some(&probe),
    );

    w.hand_over_agent().await?;

    let deposit = w
        .agent
        .call(w.mandate.id(), "request_spend")
        .args_json(w.args("deposit", w.vendor.id(), 1, 1))
        .deposit(NearToken::from_yoctonear(1))
        .gas(Gas::from_tgas(100))
        .transact()
        .await;
    let error = format!(
        "{:?}",
        deposit.err().context("attaching a deposit was accepted")?
    );
    ensure!(error.contains("DepositWithFunctionCall"), "{error}");
    ensure!(
        w.request("deposit").await?.is_null(),
        "refused deposit left a record"
    );
    w.ev.held(
        "the agent's key cannot attach a deposit",
        "protocol",
        "RPC error InvalidAccessKeyError(DepositWithFunctionCall), no record",
        None,
    );

    let wrong = w
        .agent
        .call(w.mandate.id(), "approve")
        .args_json(held_args(w, 4))
        .gas(Gas::from_tgas(100))
        .transact()
        .await;
    let error = format!(
        "{:?}",
        wrong.err().context("the agent's key called approve")?
    );
    ensure!(error.contains("MethodNameMismatch"), "{error}");
    w.ev.held(
        "the agent's key cannot call any other method (approve)",
        "protocol",
        "RPC error InvalidAccessKeyError(MethodNameMismatch)",
        None,
    );

    let owner_spend = w
        .spend_as(&w.owner, "owner-spend", w.vendor.id(), 1, 1)
        .await?;
    ensure!(failed_with(&owner_spend, "AGENT_ONLY"), "{owner_spend:?}");
    ensure!(w.request("owner-spend").await?.is_null());
    w.ev.held(
        "only the agent can request a spend; not even the owner",
        "contract",
        "panic AGENT_ONLY, no record",
        Some(&owner_spend),
    );

    let attacker = w.attacker.id().clone();
    let vendor = w.vendor.id().clone();
    let hosting = w.hosting.id().clone();
    let ghost = w.ghost.clone();
    w.expect_denied(
        "paying an unlisted account (you) is refused",
        (1, "pay-me", &attacker, 1),
        "UNLISTED",
    )
    .await?;
    w.expect_denied(
        "a request under another policy version is refused",
        (2, "old-policy", &vendor, 1),
        "POLICY_VERSION",
    )
    .await?;
    w.expect_denied(
        "a zero-amount request is refused",
        (1, "zero", &vendor, 0),
        "ZERO_AMOUNT",
    )
    .await?;

    let settled = w.spend("buy-1", &vendor, 4).await?;
    ensure!(
        settled.is_success() && settled.receipt_failures().is_empty(),
        "{settled:?}"
    );
    let record = w.request("buy-1").await?;
    ensure!(record["status"] == "SETTLED", "{record}");
    ensure!(
        w.balance(&vendor).await? - vendor_start == w.n(4),
        "vendor not paid exactly 4 steps"
    );
    ensure!(w.committed().await? == w.n(4));
    w.ev.held(
        "an in-policy purchase settles",
        "settlement",
        "SETTLED after checking the transfer result; vendor balance +4 steps exactly",
        Some(&settled),
    );

    w.expect_denied(
        "replaying a settled request is refused",
        (1, "buy-1", &vendor, 4),
        "REPLAY",
    )
    .await?;
    w.expect_denied(
        "reusing a request ID with altered arguments is refused",
        (1, "buy-1", &vendor, 9),
        "ALTERED",
    )
    .await?;
    ensure!(
        w.balance(&vendor).await? - vendor_start == w.n(4),
        "replay paid the vendor again"
    );

    let failed = w.spend("ghost-1", &ghost, 2).await?;
    ensure!(
        failed.is_success() && failed.receipt_failures().len() == 1,
        "{failed:?}"
    );
    ensure!(format!("{:?}", failed.receipt_failures()).contains("AccountDoesNotExist"));
    let record = w.request("ghost-1").await?;
    ensure!(
        record["status"] == "FAILED" && record["releases"] == 1,
        "{record}"
    );
    ensure!(
        w.committed().await? == w.n(4),
        "failed transfer kept its charge"
    );
    w.ev.held(
        "a failed transfer releases its charge exactly once",
        "settlement",
        "transfer receipt AccountDoesNotExist; FAILED, releases=1, budget back to 4 steps",
        Some(&failed),
    );

    let split_a = w.spend("split-a", &vendor, 4).await?;
    ensure!(split_a.is_success(), "{split_a:?}");
    let split_b = w.spend("split-b", &vendor, 4).await?;
    ensure!(w.request("split-a").await?["status"] == "SETTLED");
    let held: Value = split_b.clone().json()?;
    ensure!(
        held["status"] == "AWAITING_APPROVAL" && held["needs_approval"] == true,
        "{held}"
    );
    ensure!(
        w.balance(&vendor).await? - vendor_start == w.n(8),
        "held request was paid"
    );
    let summary = w.summary().await?;
    ensure!(
        summary["committed"] == w.n(8).to_string() && summary["held"] == w.n(4).to_string(),
        "{summary}"
    );
    w.ev.held(
        "splitting a payment does not dodge owner approval: past 10 steps in a period the agent waits",
        "contract",
        "split-a SETTLED, split-b AWAITING_APPROVAL (held, not charged, not paid)",
        Some(&split_b),
    );

    let altered = w
        .owner
        .call(w.mandate.id(), "approve")
        .args_json(held_args(w, 9))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    ensure!(failed_with(&altered, "BINDING_MISMATCH"), "{altered:?}");
    let mut other_invoice = held_args(w, 4);
    other_invoice["invoice"] = json!("invoice-something-else");
    let swapped = w
        .owner
        .call(w.mandate.id(), "approve")
        .args_json(other_invoice)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    ensure!(failed_with(&swapped, "BINDING_MISMATCH"), "{swapped:?}");
    ensure!(w.request("split-b").await?["status"] == "AWAITING_APPROVAL");
    w.ev.held(
        "an approval with an altered amount is refused",
        "contract",
        "panic BINDING_MISMATCH, request still held",
        Some(&altered),
    );
    w.ev.held(
        "an approval for a different invoice is refused",
        "contract",
        "panic BINDING_MISMATCH, request still held",
        Some(&swapped),
    );

    let approved = w
        .owner
        .call(w.mandate.id(), "approve")
        .args_json(held_args(w, 4))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    ensure!(
        approved.is_success() && approved.receipt_failures().is_empty(),
        "{approved:?}"
    );
    ensure!(w.request("split-b").await?["status"] == "SETTLED");
    ensure!(w.balance(&vendor).await? - vendor_start == w.n(12));
    w.ev.held(
        "the owner's exact approval pays the held request once",
        "settlement",
        "SETTLED; vendor balance +12 steps in total",
        Some(&approved),
    );

    w.expect_denied(
        "exceeding a payee's cap is refused",
        (1, "big-vendor", &vendor, 20),
        "PAYEE_CAP",
    )
    .await?;

    w.expect_denied(
        "exceeding the total cap is refused",
        (1, "hosting-29", &hosting, 29),
        "TOTAL_CAP",
    )
    .await?;

    let hold = w.spend("hosting-1", &hosting, 25).await?;
    ensure!(hold.clone().json::<Value>()?["status"] == "AWAITING_APPROVAL");
    ensure!(w.committed().await? == w.n(12));

    let rejected = w
        .owner
        .call(w.mandate.id(), "reject")
        .args_json(json!({"request_id": "hosting-1"}))
        .transact()
        .await?;
    let record: Value = rejected.clone().json()?;
    ensure!(
        record["status"] == "REJECTED" && record["releases"] == 0,
        "{record}"
    );
    let summary = w.summary().await?;
    ensure!(
        summary["committed"] == w.n(12).to_string() && summary["held"] == "0",
        "{summary}"
    );
    let again = w
        .owner
        .call(w.mandate.id(), "reject")
        .args_json(json!({"request_id": "hosting-1"}))
        .transact()
        .await?;
    ensure!(failed_with(&again, "NOT_AWAITING_APPROVAL"), "{again:?}");
    ensure!(
        w.committed().await? == w.n(12),
        "second reject changed the budget"
    );
    w.ev.held(
        "a rejected request is closed once and nothing is paid",
        "contract",
        "REJECTED, held back to 0, committed unchanged; a second reject panics NOT_AWAITING_APPROVAL",
        Some(&rejected),
    );

    let callback = w
        .owner
        .call(w.mandate.id(), "on_transfer")
        .args_json(json!({"index": 0}))
        .transact()
        .await?;
    ensure!(failed_with(&callback, "is private"), "{callback:?}");
    w.ev.held(
        "nobody else can call the settlement callback",
        "contract",
        "panic: Method on_transfer is private",
        Some(&callback),
    );

    let summary = w.summary().await?;
    ensure!(
        w.balance(&attacker).await? == attacker_start,
        "the unlisted account received money"
    );
    ensure!(w.balance(&hosting).await? == hosting_start);
    ensure!(w.balance(&vendor).await? - vendor_start == w.n(12));
    ensure!(w.worker.view_account(&ghost).await.is_err());
    ensure!(
        summary["committed"] == w.n(12).to_string()
            && summary["payee_committed"][vendor.as_str()] == w.n(12).to_string(),
        "{summary}"
    );
    w.ev.held(
        "end state: you received nothing; only settled requests moved money",
        "state",
        "attacker +0, hosting +0, vendor +12 steps = 3 settled requests; committed 12 of 40",
        None,
    );
    Ok(())
}

/// Prints every attempt the mandate recorded, as a table.
pub async fn print_attempts(mandate: &Contract) -> Result<()> {
    let attempts: Vec<Value> = mandate
        .view("attempts")
        .args_json(json!({"from": 0, "limit": 100}))
        .await?
        .json()?;
    println!("\nattempts recorded by {}", mandate.id());
    println!(
        "{:>3}  {:<12} {:<9} {:>11}  {:<18} {}",
        "#", "request_id", "payee", "NEAR", "status", "reason"
    );
    for a in attempts {
        let text = |key: &str| a[key].as_str().unwrap_or("").to_string();
        let amount: u128 = text("amount").parse()?;
        println!(
            "{:>3}  {:<12} {:<9} {:>11.4}  {:<18} {}",
            a["index"].as_u64().unwrap_or(0),
            text("request_id"),
            text("payee").split('.').next().unwrap_or(""),
            amount as f64 / NEAR as f64,
            text("status"),
            text("reason"),
        );
    }
    Ok(())
}
