//! Invariant tests for the spending-mandate guard (self-verification). Seeded random sequences of
//! requests, owner approvals and rejections, period crossings and closes run against fresh sandbox
//! mandates with random policies. After every step the invariants are checked against the chain:
//! the step's receipts, account balances, `mandate()` and every stored attempt.
//!
//! - I1 every transfer out of the mandate goes to an allowlisted payee within its cap, for a request
//!   settled or failed in that step, or to `refund_to` from `close` (the owner's, or anyone's from
//!   `expires_at` on);
//! - I2 `committed` ≤ total cap, each payee's committed ≤ its cap, the agent's own commitments in
//!   each period ≤ `approval_threshold`;
//! - I3 `committed` and each payee's committed equal the sum of settled requests, and so do the
//!   payees' balances; a failed transfer is released exactly once; `held` is the sum awaiting;
//! - I4 the mandate's balance falls only by settled payments and the close refund;
//! - I5 a request denied EXHAUSTED in period N is accepted in a later period if it still fits.
//!
//! `npm run verify` runs 64 sequences × 40 steps with seed 494; `npm run verify:fuzz` runs 2,048
//! with a random seed. PREAVIS_FUZZ_SEED, PREAVIS_FUZZ_SEQUENCES and PREAVIS_FUZZ_STEPS override.
//! A violation prints the seed and the shortest failing sequence found by removing steps.
mod common;

use anyhow::{Context, Result, anyhow, bail};
use common::*;
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, Contract, Worker};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Sequences run at once, each against its own mandate, in one sandbox.
const CONCURRENCY: u64 = 64;

/// SplitMix64: everything a sequence does follows from its seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

#[derive(Clone, Copy, Debug)]
enum Who {
    Owner,
    Agent,
    Stranger,
}

/// One step, symbolic: payees, amounts and IDs are resolved against the mandate when it runs.
#[derive(Clone, Debug)]
enum Op {
    /// `id`: fresh, `Reuse` an earlier ID with new arguments, `Replay` an earlier request exactly,
    /// or `Bad` (empty or 65 bytes). `version` and `invoice` 0 are the right ones.
    Spend {
        id: Id,
        pick: u64,
        payee: u64,
        amount: u64,
        units: u64,
        version: u64,
        invoice: u64,
        who: Who,
    },
    /// `bind` 0 repeats the held request exactly; 1–5 alter amount, invoice, payee or version.
    Approve {
        pick: u64,
        bind: u64,
        who: Who,
    },
    Reject {
        pick: u64,
        who: Who,
    },
    /// Waits for the next period and resends the last EXHAUSTED request (I5), if there is one.
    NextPeriod,
    Close {
        who: Who,
    },
}

#[derive(Clone, Copy, Debug)]
enum Id {
    Fresh,
    Reuse,
    Replay,
    Bad,
}

/// The random policy of one sequence. `unit` is 0.1 NEAR, or 1 yocto in one sequence of ten.
struct Plan {
    unit: u128,
    version: u32,
    /// vendor, hosting, ghost (never created, so a transfer fails), an implicit account.
    caps: [u128; 4],
    total: u128,
    threshold: u128,
    period_blocks: u64,
    expires_in: u64,
    max_payments: u64,
    max_denials: u64,
    ops: Vec<Op>,
}

fn plan(seed: u64, sequence: u64, steps: u64) -> Plan {
    let mut r = Rng(seed ^ sequence.wrapping_mul(0xA24B_AED4_963E_E407));
    let unit = if r.chance(10) { 1 } else { NEAR / 10 };
    let total = u128::from(r.range(4, 40)) * unit;
    let who = |r: &mut Rng| match r.next() % 10 {
        0 => Who::Agent,
        1 => Who::Stranger,
        _ => Who::Owner,
    };
    let mut ops: Vec<Op> = (0..steps.saturating_sub(2))
        .map(|_| match r.next() % 100 {
            0..=59 => Op::Spend {
                id: [
                    Id::Fresh,
                    Id::Fresh,
                    Id::Fresh,
                    Id::Reuse,
                    Id::Replay,
                    Id::Replay,
                ][(r.next() % 6) as usize],
                pick: r.next(),
                payee: if r.chance(75) { r.next() % 4 } else { r.next() },
                amount: if r.chance(50) { 0 } else { r.next() },
                units: r.range(1, 4),
                version: if r.chance(85) { 0 } else { r.next() },
                invoice: if r.chance(90) { 0 } else { r.next() },
                who: if r.chance(95) {
                    Who::Agent
                } else {
                    who(&mut r)
                },
            },
            60..=61 => Op::Spend {
                id: Id::Bad,
                pick: r.next(),
                payee: 0,
                amount: 0,
                units: 1,
                version: 0,
                invoice: 0,
                who: Who::Agent,
            },
            62..=78 => Op::Approve {
                pick: r.next(),
                bind: if r.chance(60) { 0 } else { r.next() },
                who: who(&mut r),
            },
            79..=86 => Op::Reject {
                pick: r.next(),
                who: who(&mut r),
            },
            87..=98 => Op::NextPeriod,
            _ => Op::Close {
                who: if r.chance(50) {
                    Who::Owner
                } else {
                    Who::Stranger
                },
            },
        })
        .collect();
    // Every sequence ends with a stranger's close (refused before expires_at) and the owner's.
    ops.extend([
        Op::Close { who: Who::Stranger },
        Op::Close { who: Who::Owner },
    ]);
    Plan {
        unit,
        version: r.range(1, 5) as u32,
        caps: [
            r.range(2, 20),
            r.range(2, 20),
            r.range(1, 10),
            r.range(1, 10),
        ]
        .map(|c| u128::from(c) * unit),
        total,
        threshold: u128::from(r.range(0, 40)) * unit % (total + 1),
        period_blocks: r.range(10, 30),
        expires_in: r.range(100, 900),
        max_payments: r.range(1, 2),
        max_denials: r.range(0, 4),
        ops,
    }
}

fn num(v: &Value) -> u128 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn violation(text: String) -> anyhow::Error {
    anyhow!("INVARIANT VIOLATED: {text}")
}

macro_rules! holds {
    ($cond:expr, $($text:tt)+) => {
        if !$cond {
            return Err(violation(format!($($text)+)));
        }
    };
}

/// What happened, short: `SETTLED`, `DENIED EXHAUSTED`, `panic CLOSED`, …
fn outcome(r: &ExecutionFinalResult) -> String {
    if !r.is_success() {
        return format!("panic {}", failures(r).join(","));
    }
    match r.clone().json::<Value>() {
        Ok(a) if a["status"].is_string() => {
            let reason = a["reason"]
                .as_str()
                .map(|r| format!(" {r}"))
                .unwrap_or_default();
            format!("{}{reason}", a["status"].as_str().unwrap_or(""))
        }
        _ => "ok".into(),
    }
}

/// One sequence's mandate, accounts and what the chain showed after the previous step.
struct Run {
    worker: Worker<Sandbox>,
    p: Plan,
    owner: Account,
    agent: Account,
    stranger: Account,
    mandate: Contract,
    refund_to: near_workspaces::AccountId,
    /// Allowlisted (vendor, hosting, ghost, implicit), then unlisted: look-alikes, sub-accounts of
    /// payees, an implicit account, refund_to, owner, agent, stranger, the mandate itself, an
    /// invalid (upper-case) name, and an unlisted account that exists.
    payees: Vec<String>,
    caps: BTreeMap<String, u128>,
    expires_at: u64,
    summary: Value,
    attempts: Vec<Value>,
    balances: BTreeMap<String, u128>,
    spends: Vec<Value>,
    /// The last request denied EXHAUSTED and its period.
    exhausted: Option<(Value, u64)>,
    log: Vec<String>,
}

/// `subaccount`, retried when concurrent sequences race for the parent's nonce.
async fn create(parent: &Account, name: &str, balance: NearToken) -> Result<Account> {
    let mut tries = 0;
    loop {
        match subaccount(parent, name, balance).await {
            Err(e) if tries < 8 && format!("{e:?}").contains("InvalidNonce") => tries += 1,
            created => return created,
        }
    }
}

struct Failure {
    step: usize,
    error: anyhow::Error,
    log: Vec<String>,
}

impl Run {
    async fn new(
        worker: &Worker<Sandbox>,
        root: &Account,
        wasm: &[u8],
        name: &str,
        p: Plan,
    ) -> Result<Self> {
        let owner = create(root, name, NearToken::from_near(60)).await?;
        let small = NearToken::from_millinear(50);
        let (agent, stranger, vendor, hosting, attacker, treasury) = tokio::try_join!(
            create(&owner, "agent", NearToken::from_near(5)),
            create(&owner, "stranger", NearToken::from_near(2)),
            create(&owner, "vendor", small),
            create(&owner, "hosting", small),
            create(&owner, "attacker", small),
            create(&owner, "treasury", small),
        )?;
        let o = owner.id();
        let hex = |tag: &str| sha256_hex(format!("{name}-{tag}").as_bytes());
        let (ghost, implicit) = (format!("ghost.{o}"), hex("implicit"));
        let caps: BTreeMap<String, u128> = [
            vendor.id().to_string(),
            hosting.id().to_string(),
            ghost.clone(),
            implicit.clone(),
        ]
        .into_iter()
        .zip(p.caps)
        .collect();
        let expires_at = worker.view_block().await?.height() + p.expires_in;
        let policy = json!({
            "version": p.version,
            "payees": caps.iter().map(|(k, v)| (k.clone(), json!(v.to_string()))).collect::<serde_json::Map<_, _>>(),
            "total_cap": p.total.to_string(), "approval_threshold": p.threshold.to_string(),
            "period_blocks": p.period_blocks, "expires_at": expires_at,
            "max_payments": p.max_payments, "max_denials": p.max_denials,
        });
        let balance = NearToken::from_yoctonear(p.total + 10 * NEAR);
        let (mandate, _) = deploy_mandate(
            &owner,
            "mandate",
            balance,
            wasm,
            agent.id(),
            treasury.id(),
            policy,
        )
        .await?;
        let payees = [
            vendor.id().to_string(),
            hosting.id().to_string(),
            ghost,
            implicit,
            format!("vend0r.{o}"),
            format!("vendorr.{o}"),
            format!("{}.x", vendor.id()),
            format!("pay.{}", vendor.id()),
            format!("a.{}", hosting.id()),
            hex("unlisted"),
            treasury.id().to_string(),
            o.to_string(),
            agent.id().to_string(),
            stranger.id().to_string(),
            mandate.id().to_string(),
            vendor.id().to_string().to_uppercase(),
            attacker.id().to_string(),
        ]
        .to_vec();
        let mut run = Run {
            worker: worker.clone(),
            p,
            owner,
            agent,
            stranger,
            mandate,
            refund_to: treasury.id().clone(),
            payees,
            caps,
            expires_at,
            summary: Value::Null,
            attempts: vec![],
            balances: BTreeMap::new(),
            spends: vec![],
            exhausted: None,
            log: vec![],
        };
        // The gas refunds of the mandate's own deploy and init arrive a few blocks later.
        for _ in 0..20 {
            let before = run.watched().await?;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            run.balances = run.watched().await?;
            if run.balances == before {
                break;
            }
        }
        run.summary = run.mandate.view("mandate").await?.json()?;
        Ok(run)
    }

    /// Accounts whose balance only transfers can change: the mandate, vendor, hosting, the
    /// implicit payee (0 until created), refund_to and the unlisted account that exists.
    async fn watched(&self) -> Result<BTreeMap<String, u128>> {
        let ids = [
            self.mandate.id().as_str(),
            &self.payees[0],
            &self.payees[1],
            &self.payees[3],
            self.refund_to.as_str(),
            &self.payees[16],
        ];
        let view = |id: &str| {
            let id = id.parse::<near_workspaces::AccountId>();
            async move {
                Ok::<u128, anyhow::Error>(
                    self.worker
                        .view_account(&id?)
                        .await
                        .map_or(0, |a| a.balance.as_yoctonear()),
                )
            }
        };
        let got = tokio::try_join!(
            view(ids[0]),
            view(ids[1]),
            view(ids[2]),
            view(ids[3]),
            view(ids[4]),
            view(ids[5])
        )?;
        let got = [got.0, got.1, got.2, got.3, got.4, got.5];
        Ok(ids.iter().map(|id| id.to_string()).zip(got).collect())
    }

    fn account(&self, who: Who) -> &Account {
        match who {
            Who::Owner => &self.owner,
            Who::Agent => &self.agent,
            Who::Stranger => &self.stranger,
        }
    }

    async fn height(&self, r: &ExecutionFinalResult) -> Result<u64> {
        let block = r
            .receipt_outcomes()
            .first()
            .context("no receipt")?
            .block_hash;
        Ok(self.worker.view_block().block_hash(block).await?.height())
    }

    async fn call(&self, who: Who, method: &str, args: &Value) -> Result<ExecutionFinalResult> {
        Ok(self
            .account(who)
            .call(self.mandate.id(), method)
            .args_json(args)
            .gas(Gas::from_tgas(100))
            .transact()
            .await?)
    }

    fn amount(&self, kind: u64, units: u64, payee: &str) -> u128 {
        let s = &self.summary;
        let cap = self.caps.get(payee).copied().unwrap_or(self.p.caps[0]);
        let left = cap.saturating_sub(num(&s["payee_committed"][payee]));
        let total_left = self.p.total.saturating_sub(num(&s["committed"]));
        let own_left = self.p.threshold.saturating_sub(num(&s["period_committed"]));
        let units = u128::from(units);
        [
            units * self.p.unit,
            0,
            1,
            cap - 1,
            cap,
            cap + 1,
            left,
            left + 1,
            total_left,
            total_left + 1,
            own_left,
            own_left + 1,
            u128::MAX,
            u128::MAX - 1,
            1 << 127,
            units,
        ][(kind % 16) as usize]
    }

    /// The held request to approve or reject, else any stored request, else an unknown ID.
    fn target(&self, pick: u64) -> Value {
        let held: Vec<&Value> = self
            .attempts
            .iter()
            .filter(|a| a["status"] == "AWAITING_APPROVAL")
            .collect();
        let pool = if held.is_empty() {
            self.attempts.iter().collect()
        } else {
            held
        };
        match pool.len() {
            0 => {
                json!({"request_id": "unknown", "payee": self.payees[0], "amount": "1", "policy_version": self.p.version, "invoice": ""})
            }
            n => pool[(pick % n as u64) as usize].clone(),
        }
    }

    /// Runs one step; returns its description, its transaction and, for an I5 resend, whether the
    /// request still fits the caps and the period it was denied EXHAUSTED in.
    async fn step(
        &mut self,
        n: usize,
        op: &Op,
    ) -> Result<(String, Option<ExecutionFinalResult>, Option<(bool, u64)>)> {
        let bound = |a: &Value| json!({"request_id": a["request_id"], "payee": a["payee"], "amount": a["amount"], "policy_version": a["policy_version"], "invoice": a["invoice"]});
        Ok(match *op {
            Op::Spend {
                id,
                pick,
                payee,
                amount,
                units,
                version,
                invoice,
                who,
            } => {
                let earlier = (!self.spends.is_empty())
                    .then(|| self.spends[(pick % self.spends.len() as u64) as usize].clone());
                let args = match (id, earlier) {
                    (Id::Replay, Some(e)) => e,
                    (id, earlier) => {
                        let id = match (id, earlier) {
                            (Id::Reuse, Some(e)) => {
                                e["request_id"].as_str().unwrap_or("").to_string()
                            }
                            (Id::Bad, _) => "x".repeat(if pick % 2 == 0 { 0 } else { 65 }),
                            _ => format!("r{n}"),
                        };
                        let payee =
                            self.payees[(payee % self.payees.len() as u64) as usize].clone();
                        let v = self.p.version;
                        let version = [v, v + 1, v - 1, 0, u32::MAX][(version % 5) as usize];
                        json!({
                            "request_id": id, "payee": payee, "amount": self.amount(amount, units, &payee).to_string(),
                            "policy_version": version,
                            "invoice": match invoice % 4 { 0 => format!("inv-{id}"), 1 => sha256_hex(id.as_bytes()), 2 => String::new(), _ => "i".repeat(65) },
                        })
                    }
                };
                self.spends.push(args.clone());
                let r = self.call(who, "request_spend", &args).await?;
                if let Ok(a) = r.clone().json::<Value>() {
                    if a["reason"] == "EXHAUSTED" {
                        self.exhausted = Some((args.clone(), a["period"].as_u64().unwrap_or(0)));
                    }
                }
                (format!("{who:?} request_spend {args}"), Some(r), None)
            }
            Op::Approve { pick, bind, who } => {
                let mut args = bound(&self.target(pick));
                match bind % 6 {
                    1 => args["amount"] = json!(num(&args["amount"]).wrapping_add(1).to_string()),
                    2 => args["amount"] = json!(num(&args["amount"]).saturating_sub(1).to_string()),
                    3 => args["invoice"] = json!("another-invoice"),
                    4 => {
                        args["payee"] = json!(if args["payee"] == self.payees[0] {
                            &self.payees[1]
                        } else {
                            &self.payees[0]
                        })
                    }
                    5 => args["policy_version"] = json!(self.p.version + 1),
                    _ => {}
                }
                let r = self.call(who, "approve", &args).await?;
                (
                    format!("{who:?} approve (binding {}) {args}", bind % 6),
                    Some(r),
                    None,
                )
            }
            Op::Reject { pick, who } => {
                let args = json!({"request_id": self.target(pick)["request_id"]});
                let r = self.call(who, "reject", &args).await?;
                (format!("{who:?} reject {args}"), Some(r), None)
            }
            Op::NextPeriod => {
                // Periods of 10–30 blocks also pass on their own, every few steps.
                let now = self.worker.view_block().await?.height();
                let Some((mut args, period)) = self.exhausted.take() else {
                    return Ok((
                        "next period: no EXHAUSTED request to resend".into(),
                        None,
                        None,
                    ));
                };
                if self.summary["closed"] == true || now >= self.expires_at {
                    return Ok((
                        "next period: skipped (closed or expired)".into(),
                        None,
                        None,
                    ));
                }
                let resets_at = self.summary["resets_at"].as_u64().context("resets_at")?;
                while self.worker.view_block().await?.height() < resets_at {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                // I5: the request denied EXHAUSTED, in the new period; under a new ID only if
                // its own was taken since.
                let id = &args["request_id"];
                if self
                    .attempts
                    .iter()
                    .any(|a| a["request_id"] == *id && a["status"] != "DENIED")
                {
                    args["request_id"] = json!(format!("i5-{n}"));
                }
                let s: Value = self.mandate.view("mandate").await?.json()?;
                let (payee, amount) = (args["payee"].as_str().unwrap_or(""), num(&args["amount"]));
                let fits = amount
                    <= self.caps[payee].saturating_sub(num(&s["payee_committed"][payee]))
                    && amount <= self.p.total.saturating_sub(num(&s["committed"]));
                self.spends.push(args.clone());
                let r = self.call(Who::Agent, "request_spend", &args).await?;
                (
                    format!(
                        "next period (block {resets_at}), resend of the EXHAUSTED request from period {period}: {args}"
                    ),
                    Some(r),
                    Some((fits, period)),
                )
            }
            Op::Close { who } => {
                let r = self.call(who, "close", &json!({})).await?;
                (format!("{who:?} close"), Some(r), None)
            }
        })
    }

    /// Checks I1–I5 against the chain after a step.
    async fn check(
        &mut self,
        op: &Op,
        r: Option<&ExecutionFinalResult>,
        i5: Option<(bool, u64)>,
    ) -> Result<()> {
        let (summary, attempts, balances, transfers) = tokio::try_join!(
            async { Ok::<Value, anyhow::Error>(self.mandate.view("mandate").await?.json()?) },
            async {
                Ok(self
                    .mandate
                    .view("attempts")
                    .args_json(json!({"from": 0, "limit": 1000}))
                    .await?
                    .json::<Vec<Value>>()?)
            },
            self.watched(),
            async {
                match r {
                    Some(r) => transfers(&self.worker, r).await,
                    None => Ok(vec![]),
                }
            },
        )?;
        let mandate = self.mandate.id().as_str();

        // Requests settled or failed in this step.
        let mut moved: Vec<(String, u128, bool)> = attempts
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                let before = self.attempts.get(*i).map(|b| b["status"].clone());
                (a["status"] == "SETTLED" || a["status"] == "FAILED")
                    && before != Some(a["status"].clone())
            })
            .map(|(_, a)| {
                (
                    a["payee"].as_str().unwrap_or("").to_string(),
                    num(&a["amount"]),
                    a["status"] == "SETTLED",
                )
            })
            .collect();
        let settled_now: u128 = moved.iter().filter(|m| m.2).map(|m| m.1).sum();

        // I1: where money left the mandate.
        let mut refund = 0;
        for t in transfers.iter().filter(|t| t["from"] == mandate) {
            let (to, amount) = (t["to"].as_str().unwrap_or(""), num(&t["amount"]));
            if to == self.refund_to.as_str() {
                let r = r.context("transfer without a transaction")?;
                let by_owner = matches!(op, Op::Close { who: Who::Owner });
                let expired = self.height(r).await? >= self.expires_at;
                holds!(
                    matches!(op, Op::Close { .. }) && (by_owner || expired),
                    "I1 {amount} yocto to refund_to outside a permitted close"
                );
                let closed = events(r).into_iter().find(|e| e["event"] == "closed");
                holds!(
                    closed.is_some_and(|e| num(&e["data"][0]["amount"]) == amount),
                    "I1 refund of {amount} differs from the closed event"
                );
                refund += amount;
                continue;
            }
            let cap = self.caps.get(to).copied();
            holds!(
                cap.is_some_and(|c| amount <= c),
                "I1 transfer of {amount} yocto to {to}, not an allowlisted payee within its cap"
            );
            let at = moved.iter().position(|m| m.0 == to && m.1 == amount);
            holds!(
                at.is_some(),
                "I1 transfer of {amount} yocto to {to} without a request settled or failed in this step"
            );
            moved.remove(at.unwrap());
        }
        holds!(
            moved.is_empty(),
            "I1 requests settled or failed without their transfer: {moved:?}"
        );
        if let (Some(r), Op::Close { who: Who::Stranger }) = (r, op) {
            let expired = self.height(r).await? >= self.expires_at;
            holds!(
                r.is_success() == expired,
                "I1 a stranger's close {} at expired={expired}",
                outcome(r)
            );
        }

        // I2: the caps, from the contract's totals and from the attempts themselves.
        let sum = |pred: &dyn Fn(&Value) -> bool| -> u128 {
            attempts
                .iter()
                .filter(|a| pred(a))
                .map(|a| num(&a["amount"]))
                .sum()
        };
        let committed = num(&summary["committed"]);
        holds!(
            committed <= self.p.total,
            "I2 committed {committed} > total cap {}",
            self.p.total
        );
        for (payee, cap) in &self.caps {
            let c = num(&summary["payee_committed"][payee]);
            holds!(c <= *cap, "I2 {payee} committed {c} > cap {cap}");
        }
        holds!(
            num(&summary["period_committed"]) <= self.p.threshold,
            "I2 period_committed {} > approval_threshold {}",
            summary["period_committed"],
            self.p.threshold
        );
        let mut own: BTreeMap<u64, u128> = BTreeMap::new();
        for a in attempts
            .iter()
            .filter(|a| a["status"] == "SETTLED" && a["needs_approval"] == false)
        {
            *own.entry(a["period"].as_u64().unwrap_or(0)).or_default() += num(&a["amount"]);
        }
        holds!(
            own.values().all(|c| *c <= self.p.threshold),
            "I2 agent-committed per period {own:?} > approval_threshold {}",
            self.p.threshold
        );

        // I3: totals equal what settled; releases happen exactly once; payees got exactly that.
        holds!(
            !attempts.iter().any(|a| a["status"] == "PENDING"),
            "I3 a request is still PENDING after its transaction finished"
        );
        let settled = sum(&|a| a["status"] == "SETTLED");
        holds!(
            committed == settled,
            "I3 committed {committed} != settled {settled}"
        );
        holds!(
            num(&summary["held"]) == sum(&|a| a["status"] == "AWAITING_APPROVAL"),
            "I3 held {} != sum awaiting",
            summary["held"]
        );
        for payee in self.caps.keys() {
            let (c, s) = (
                num(&summary["payee_committed"][payee]),
                sum(&|a| a["status"] == "SETTLED" && a["payee"] == payee.as_str()),
            );
            holds!(c == s, "I3 {payee} committed {c} != settled {s}");
        }
        for a in &attempts {
            let releases = u64::from(a["status"] == "FAILED");
            holds!(
                a["releases"].as_u64() == Some(releases),
                "I3 {} released {} times",
                a["request_id"],
                a["releases"]
            );
        }
        for (id, before) in self
            .balances
            .iter()
            .filter(|(id, _)| id.as_str() != mandate)
        {
            let expected = if *id == self.refund_to.as_str() {
                refund as i128
            } else {
                settled_to(&attempts, id) as i128 - settled_to(&self.attempts, id) as i128
            };
            let change = balances[id] as i128 - *before as i128;
            holds!(
                change == expected,
                "I3 {id} balance changed by {change} yocto, expected {expected}"
            );
        }

        // I4: the mandate's own balance.
        let (before, after) = (self.balances[mandate], balances[mandate]);
        let out = settled_now + refund;
        // It may gain at most its share of the gas its own receipts burnt (0 in the sandbox).
        let burnt: u128 = r.map_or(0, |r| {
            r.outcomes()
                .iter()
                .filter(|o| o.executor_id.as_str() == mandate)
                .map(|o| o.tokens_burnt.as_yoctonear())
                .sum()
        });
        holds!(
            after + out >= before && after + out - before <= burnt,
            "I4 mandate balance {before} → {after}, settled {settled_now} + refund {refund}"
        );

        // I5: the EXHAUSTED request in a later period.
        if let (Some((fits, period)), Some(r)) = (i5, r) {
            let a = r.clone().json::<Value>().unwrap_or_default();
            let expired = !r.is_success() && self.height(r).await? >= self.expires_at;
            let accepted = ["SETTLED", "FAILED", "AWAITING_APPROVAL"]
                .contains(&a["status"].as_str().unwrap_or(""));
            holds!(
                !fits || expired || accepted && a["period"].as_u64() > Some(period),
                "I5 a request denied EXHAUSTED in period {period} was {} in period {} while within policy",
                outcome(r),
                a["period"]
            );
        }

        self.summary = summary;
        self.attempts = attempts;
        self.balances = balances;
        Ok(())
    }

    /// No transfer ever created an account the mandate must not pay.
    async fn end(&self) -> Result<()> {
        for name in [2, 4, 5, 6, 7, 8, 9].map(|i| &self.payees[i]) {
            holds!(
                self.worker.view_account(&name.parse()?).await.is_err(),
                "I1 {name} exists"
            );
        }
        Ok(())
    }
}

fn settled_to(attempts: &[Value], payee: &str) -> u128 {
    attempts
        .iter()
        .filter(|a| a["status"] == "SETTLED" && a["payee"] == payee)
        .map(|a| num(&a["amount"]))
        .sum()
}

/// Runs `ops` against a fresh mandate under `name`, checking after every step.
async fn run(
    worker: &Worker<Sandbox>,
    root: &Account,
    wasm: &[u8],
    name: &str,
    p: Plan,
) -> Result<(Vec<String>, BTreeMap<String, u64>), Failure> {
    let ops = p.ops.clone();
    let mut run = Run::new(worker, root, wasm, name, p)
        .await
        .map_err(|error| Failure {
            step: 0,
            error,
            log: vec![],
        })?;
    let mut seen = BTreeMap::new();
    for (n, op) in ops.iter().enumerate() {
        let result = run.step(n, op).await;
        let (text, r, i5) = result.map_err(|error| Failure {
            step: n,
            error,
            log: run.log.clone(),
        })?;
        let got = r.as_ref().map(outcome).unwrap_or_else(|| "-".into());
        run.log.push(format!("{:>3}. {text} → {got}", n + 1));
        let kind = match *op {
            Op::Spend { who, .. } => format!("{who:?} request_spend"),
            Op::Approve { who, bind, .. } => format!(
                "{who:?} approve, {} binding",
                if bind % 6 == 0 { "exact" } else { "altered" }
            ),
            Op::Reject { who, .. } => format!("{who:?} reject"),
            Op::NextPeriod if i5.is_some() => "next period, EXHAUSTED request resent (I5)".into(),
            Op::NextPeriod => "next period".into(),
            Op::Close { who } => format!("{who:?} close"),
        };
        *seen.entry(format!("{kind} → {got}")).or_insert(0) += 1;
        if let Err(error) = run.check(op, r.as_ref(), i5).await {
            return Err(Failure {
                step: n,
                error,
                log: run.log.clone(),
            });
        }
    }
    run.end().await.map_err(|error| Failure {
        step: ops.len(),
        error,
        log: run.log.clone(),
    })?;
    Ok((run.log, seen))
}

/// Removes steps one at a time while the sequence still violates an invariant.
async fn shrink(
    worker: &Worker<Sandbox>,
    root: &Account,
    wasm: &[u8],
    seed: u64,
    sequence: u64,
    steps: u64,
    first: Failure,
) -> Failure {
    let mut ops = plan(seed, sequence, steps).ops;
    ops.truncate(first.step + 1);
    let mut best = first;
    let mut i = ops.len();
    for attempt in 0..80 {
        if i == 0 {
            break;
        }
        i -= 1;
        let mut candidate = ops.clone();
        candidate.remove(i);
        let p = Plan {
            ops: candidate.clone(),
            ..plan(seed, sequence, steps)
        };
        if let Err(f) = run(worker, root, wasm, &format!("shrink{attempt}"), p).await {
            if f.error.to_string().starts_with("INVARIANT") {
                ops = candidate;
                ops.truncate(f.step + 1);
                i = i.min(ops.len());
                best = f;
            }
        }
    }
    best
}

#[tokio::test]
async fn invariants() -> Result<()> {
    let long = std::env::var("PREAVIS_FUZZ").as_deref() == Ok("long");
    let var = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let random = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos() as u64;
    let seed = var("PREAVIS_FUZZ_SEED").unwrap_or(if long { random } else { 494 });
    let sequences = var("PREAVIS_FUZZ_SEQUENCES").unwrap_or(if long { 2048 } else { 64 });
    let steps = var("PREAVIS_FUZZ_STEPS").unwrap_or(40).max(2);
    println!("invariant tests: {sequences} sequences × {steps} steps, seed {seed}");
    let wasm = match std::env::var("PREAVIS_FUZZ_FEATURE") {
        Ok(feature) => wasm(Some(&feature))?,
        Err(_) => contract_wasm()?,
    };
    let started = std::time::Instant::now();
    let mut seen: BTreeMap<String, u64> = BTreeMap::new();
    let mut transactions = 0;
    for batch in (0..sequences).step_by(CONCURRENCY as usize) {
        let worker = near_workspaces::sandbox().await?;
        let root = worker.root_account()?;
        let mut set = tokio::task::JoinSet::new();
        for sequence in batch..(batch + CONCURRENCY).min(sequences) {
            let (worker, root, wasm) = (worker.clone(), root.clone(), wasm.clone());
            set.spawn(async move {
                let p = plan(seed, sequence, steps);
                (
                    sequence,
                    run(&worker, &root, &wasm, &format!("s{sequence}"), p).await,
                )
            });
        }
        let mut failed: Option<(u64, Failure)> = None;
        while let Some(joined) = set.join_next().await {
            match joined? {
                (_, Ok((_, counts))) => {
                    for (k, v) in counts {
                        transactions += if k.ends_with("→ -") { 0 } else { v };
                        *seen.entry(k).or_default() += v;
                    }
                }
                (s, Err(f)) if failed.as_ref().is_none_or(|(t, _)| s < *t) => failed = Some((s, f)),
                _ => {}
            }
        }
        if let Some((sequence, f)) = failed {
            if !f.error.to_string().starts_with("INVARIANT") {
                bail!(
                    "sequence {sequence} (seed {seed}) could not run, after {} steps: {:#}",
                    f.log.len(),
                    f.error
                );
            }
            println!(
                "\n{:#}\nsequence {sequence} of seed {seed}, step {}; shrinking…",
                f.error,
                f.step + 1
            );
            let small = shrink(&worker, &root, &wasm, seed, sequence, steps, f).await;
            println!(
                "\nminimal failing sequence ({} steps), policy {:?}:",
                small.log.len(),
                policy_line(&plan(seed, sequence, steps))
            );
            for line in &small.log {
                println!("{line}");
            }
            println!("  → {:#}", small.error);
            println!(
                "reproduce: PREAVIS_FUZZ_SEED={seed} PREAVIS_FUZZ_SEQUENCES={} PREAVIS_FUZZ_STEPS={steps} npm run verify:fuzz",
                sequence + 1
            );
            bail!("invariant violated (seed {seed}, sequence {sequence})");
        }
        if sequences > CONCURRENCY {
            println!(
                "  {} of {sequences} sequences: 0 invariant violations ({:.0} s)",
                (batch + CONCURRENCY).min(sequences),
                started.elapsed().as_secs_f64()
            );
        }
    }
    println!("\nsteps by outcome:");
    for (k, v) in &seen {
        println!("  {v:>7}  {k}");
    }
    println!(
        "\nPreavis invariants: {sequences} sequences × {steps} steps ({} steps, {transactions} transactions), seed {seed}, {:.0} s: 0 invariant violations.",
        sequences * steps,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn policy_line(p: &Plan) -> String {
    format!(
        "unit {} yocto, version {}, caps vendor/hosting/ghost/implicit {:?}, total {}, threshold {}, period {} blocks, expires in {} blocks, {} payments and {} stored denials per period",
        p.unit,
        p.version,
        p.caps,
        p.total,
        p.threshold,
        p.period_blocks,
        p.expires_in,
        p.max_payments,
        p.max_denials
    )
}
