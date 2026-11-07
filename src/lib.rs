//! Preavis: a spending mandate for an AI agent on NEAR.
//!
//! The mandate account holds the budget. The agent may only call `request_spend`; every request is
//! checked against a fixed policy, charged before the transfer leaves, and settled or released by a
//! private callback. The owner can only approve or reject requests held above the agent's own limit
//! per period, and close the mandate, which sends what is left to the `refund_to` account fixed at
//! init. After `expires_at` nothing more is paid and anyone may close it.
//! Every limit a flood of requests can use up (the agent's own budget, request slots, stored
//! denials) is per period, so whoever holds the agent's key can exhaust the current period only.
//! There is no method that deploys code, adds keys or edits the policy. Outgoing receipts are built
//! with the low-level `env::promise_*` calls, so the wasm imports no host function that could
//! deploy code, change keys, delete the account or stake (checked by `no_admin_surface` and by
//! `observe-testnet` on the deployed bytes).

use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    AccountId, Gas, NearToken, PanicOnDefault, PromiseError, PromiseIndex, env, near, require,
};
use std::collections::BTreeMap;

/// Longest request ID or invoice reference, in bytes.
const MAX_ID_LEN: usize = 64;
const CALLBACK_GAS: Gas = Gas::from_tgas(15);
/// Upper bounds on storage, in bytes, with every ID and account name at its 64-byte maximum and
/// NEAR's 40-byte overhead per stored entry. One stored attempt: its record (347) and, for a
/// payment, its request-ID entry (113), 469 in all.
const RECORD_BYTES: u64 = 500;
/// The contract state besides attempts: three accounts, totals and counters.
const STATE_BYTES: u64 = 600;
/// Per allowlisted payee: its cap in the policy and its committed total.
const PAYEE_BYTES: u64 = 200;

/// Fixed at initialization. A different policy means deploying a new mandate.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Policy {
    pub version: u32,
    /// Allowlisted payees and the most each may ever be paid.
    pub payees: BTreeMap<AccountId, U128>,
    /// The most the mandate may ever commit, owner-approved requests included. Held requests
    /// reserve nothing; an approval is checked against the caps as they are when it is given.
    pub total_cap: U128,
    /// The agent may commit this much on its own per period; a request beyond it waits for the owner.
    pub approval_threshold: U128,
    /// The period's length in blocks. Periods count from the block of `new`.
    pub period_blocks: u64,
    /// The block height from which nothing more is paid and anyone may call `close`.
    pub expires_at: u64,
    /// Per period: how many requests may pass the checks (paid, held or failed). Past it the
    /// mandate is EXHAUSTED until the next period. Bounds storage.
    pub max_payments: u32,
    /// Per period: how many denials are stored; later ones are only counted and emitted as events.
    pub max_denials: u32,
}

/// Everything a request is bound to. A request ID that reserved budget can't be used again:
/// resubmitting it is a replay if every field matches and an altered request otherwise.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub request_id: String,
    pub actor: AccountId,
    pub payee: AccountId,
    pub amount: U128,
    pub policy_version: u32,
    /// What the payment is for, e.g. the sha256 of the invoice. The owner's approval must repeat it.
    pub invoice: String,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Denied,
    AwaitingApproval,
    /// Charged and transfer sent; stays here (charge frozen) if the settlement callback fails.
    Pending,
    Settled,
    /// The transfer failed; the charge was released.
    Failed,
    /// The owner rejected it; the charge was released.
    Rejected,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Reason {
    Replay,
    Altered,
    PolicyVersion,
    Unlisted,
    ZeroAmount,
    PayeeCap,
    TotalCap,
    /// This period's `max_payments` is used up; see `resets_at` in `mandate()`.
    Exhausted,
}

/// One call to `request_spend`, denied or not.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Attempt {
    /// Position in `attempts`; null for a denial past the period's `max_denials`, not stored.
    pub index: Option<u32>,
    #[serde(flatten)]
    pub request: Request,
    pub status: Status,
    pub reason: Option<Reason>,
    pub needs_approval: bool,
    /// The period the request was made in.
    pub period: u64,
    /// How many times this attempt's charge was given back. Never more than 1.
    pub releases: u8,
}

#[near(serializers = [json])]
pub struct Summary {
    pub owner: AccountId,
    pub agent: AccountId,
    pub refund_to: AccountId,
    pub policy: Policy,
    pub committed: U128,
    pub auto_committed: U128,
    /// The sum of requests awaiting approval. Not part of `committed`.
    pub held: U128,
    /// The current period (0 from the block of `new`), its first block, the first block of the
    /// next one, and what the agent used in it: its own budget, request slots, stored denials.
    pub period: u64,
    pub period_start: u64,
    pub resets_at: u64,
    pub period_committed: U128,
    pub period_payments: u32,
    pub period_denials: u32,
    /// This period's request slots are used up: every request is denied EXHAUSTED until `resets_at`.
    pub exhausted: bool,
    pub expired: bool,
    pub payee_committed: BTreeMap<AccountId, U128>,
    pub attempts: u32,
    pub payments: u32,
    pub denials: u32,
    pub denials_not_stored: u64,
    pub closed: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Mandate {
    owner: AccountId,
    agent: AccountId,
    refund_to: AccountId,
    policy: Policy,
    /// In-flight and settled amounts.
    committed: u128,
    /// The part of `committed` that did not need owner approval.
    auto_committed: u128,
    /// Block height of `new`; period `n` starts at `start + n * period_blocks`.
    start: u64,
    /// The period the `period_*` counters belong to.
    period: u64,
    /// What the agent committed on its own in `period`.
    period_committed: u128,
    period_payments: u32,
    period_denials: u32,
    /// The sum of requests awaiting approval.
    held: u128,
    payee_committed: BTreeMap<AccountId, u128>,
    attempts: Vector<Attempt>,
    by_request_id: LookupMap<String, u32>,
    payments: u32,
    denials: u32,
    denials_not_stored: u64,
    closed: bool,
}

#[near]
impl Mandate {
    /// The balance must cover the total cap plus storage for every record the policy allows in
    /// every period up to `expires_at`, so requests can always be recorded and settled.
    #[init]
    #[private]
    pub fn new(owner: AccountId, agent: AccountId, refund_to: AccountId, policy: Policy) -> Self {
        require!(owner != agent, "OWNER_IS_AGENT");
        require!(
            policy.approval_threshold.0 <= policy.total_cap.0,
            "THRESHOLD_ABOVE_TOTAL"
        );
        require!(policy.period_blocks > 0, "ZERO_PERIOD");
        require!(
            policy.expires_at > env::block_height(),
            "EXPIRES_IN_THE_PAST"
        );
        let periods = (policy.expires_at - env::block_height()).div_ceil(policy.period_blocks);
        let records = (u128::from(policy.max_payments) + u128::from(policy.max_denials))
            * u128::from(periods);
        let bytes = u128::from(env::storage_usage() + STATE_BYTES)
            + u128::from(PAYEE_BYTES) * policy.payees.len() as u128
            + u128::from(RECORD_BYTES) * records;
        let reserve = env::storage_byte_cost().as_yoctonear() * bytes;
        require!(
            env::account_balance().as_yoctonear() >= policy.total_cap.0 + reserve,
            "BALANCE_BELOW_CAP_AND_STORAGE"
        );
        Self {
            owner,
            agent,
            refund_to,
            policy,
            committed: 0,
            auto_committed: 0,
            start: env::block_height(),
            period: 0,
            period_committed: 0,
            period_payments: 0,
            period_denials: 0,
            held: 0,
            payee_committed: BTreeMap::new(),
            attempts: Vector::new(b"a"),
            by_request_id: LookupMap::new(b"r"),
            payments: 0,
            denials: 0,
            denials_not_stored: 0,
            closed: false,
        }
    }

    /// Agent only. Denials return normally and are recorded; an accepted request is charged now and
    /// either paid at once or held for the owner. Returns the attempt; for a payment, the attempt
    /// as settled by `on_transfer`.
    pub fn request_spend(
        &mut self,
        request_id: String,
        payee: AccountId,
        amount: U128,
        policy_version: u32,
        invoice: String,
    ) {
        let actor = env::predecessor_account_id();
        require!(actor == self.agent, "AGENT_ONLY");
        require!(!self.closed, "CLOSED");
        require!(!self.expired(), "EXPIRED");
        self.roll();
        require!(
            !request_id.is_empty() && request_id.len() <= MAX_ID_LEN,
            "BAD_REQUEST_ID"
        );
        require!(invoice.len() <= MAX_ID_LEN, "BAD_INVOICE");
        let request = Request {
            request_id,
            actor,
            payee,
            amount,
            policy_version,
            invoice,
        };
        let mut attempt = Attempt {
            index: None,
            request,
            status: Status::Denied,
            reason: None,
            needs_approval: false,
            period: self.period,
            releases: 0,
        };
        match self.check(&attempt.request) {
            Err(reason) => {
                attempt.reason = Some(reason);
                if self.period_denials < self.policy.max_denials {
                    self.period_denials += 1;
                    self.denials += 1;
                    attempt.index = Some(self.attempts.len());
                } else {
                    self.denials_not_stored += 1;
                }
            }
            Ok(needs_approval) => {
                let index = self.attempts.len();
                attempt.needs_approval = needs_approval;
                if needs_approval {
                    self.held += attempt.request.amount.0;
                } else {
                    self.charge(&attempt);
                }
                self.period_payments += 1;
                self.payments += 1;
                self.by_request_id
                    .insert(attempt.request.request_id.clone(), index);
                attempt.index = Some(index);
                attempt.status = if needs_approval {
                    Status::AwaitingApproval
                } else {
                    Status::Pending
                };
            }
        }
        self.save(&attempt);
        match (attempt.status, attempt.index) {
            (Status::Pending, Some(index)) => env::promise_return(Self::pay(index, &attempt)),
            _ => Self::return_json(&attempt),
        }
    }

    /// Owner only, before `expires_at` and `close`. Every bound field, the invoice included, must
    /// match the held request exactly, and the amount must still fit the payee's and the total cap
    /// (a held request reserves nothing). Returns the attempt as settled by `on_transfer`.
    pub fn approve(
        &mut self,
        request_id: String,
        payee: AccountId,
        amount: U128,
        policy_version: u32,
        invoice: String,
    ) {
        let (index, mut attempt) = self.awaiting(&request_id);
        require!(!self.closed, "CLOSED");
        require!(!self.expired(), "EXPIRED");
        let bound = Request {
            request_id,
            actor: self.agent.clone(),
            payee,
            amount,
            policy_version,
            invoice,
        };
        require!(attempt.request == bound, "BINDING_MISMATCH");
        if let Err(reason) = self.fits(&attempt.request) {
            env::panic_str(&format!("{reason:?}"));
        }
        self.held -= attempt.request.amount.0;
        self.charge(&attempt);
        attempt.status = Status::Pending;
        self.save(&attempt);
        env::promise_return(Self::pay(index, &attempt));
    }

    /// Owner only, also after `close`. Nothing was charged, so nothing is released.
    pub fn reject(&mut self, request_id: String) -> Attempt {
        let (_, mut attempt) = self.awaiting(&request_id);
        self.held -= attempt.request.amount.0;
        attempt.status = Status::Rejected;
        self.save(&attempt);
        attempt
    }

    /// Owner only, or anyone from `expires_at` on. Ends the mandate: from now on nothing is
    /// requested or approved, and everything but the storage stake goes to `refund_to`, fixed at
    /// init. Call again to send anything that arrives later.
    pub fn close(&mut self) {
        let expired = self.expired();
        require!(
            expired || env::predecessor_account_id() == self.owner,
            "OWNER_ONLY"
        );
        self.closed = true;
        let keep = env::storage_byte_cost().as_yoctonear() * u128::from(env::storage_usage());
        let amount = env::account_balance().as_yoctonear().saturating_sub(keep);
        Self::event(
            "closed",
            near_sdk::serde_json::json!({"refund_to": &self.refund_to, "amount": U128(amount), "expired": expired}),
        );
        let transfer = env::promise_batch_create(&self.refund_to);
        env::promise_batch_action_transfer(transfer, NearToken::from_yoctonear(amount));
        env::promise_return(transfer);
    }

    /// Settles or releases a transfer exactly once. Anything but a pending attempt is returned
    /// unchanged. If this callback itself fails, the attempt stays pending and its charge stays
    /// frozen: there is no refund and no resend.
    #[private]
    pub fn on_transfer(&mut self, index: u32) -> Attempt {
        if cfg!(feature = "inject-callback-failure") {
            env::panic_str("INJECTED_CALLBACK_FAILURE");
        }
        let mut attempt = self.attempts[index].clone();
        if attempt.status != Status::Pending {
            return attempt;
        }
        require!(env::promise_results_count() == 1, "ONE_TRANSFER_RESULT");
        self.roll();
        attempt.status = match env::promise_result_checked(0, 0) {
            Ok(_) => Status::Settled,
            Err(PromiseError::Failed) => {
                self.release(&mut attempt);
                Status::Failed
            }
            Err(_) => env::panic_str("UNEXPECTED_TRANSFER_RESULT"),
        };
        self.save(&attempt);
        attempt
    }

    pub fn mandate(&self) -> Summary {
        let period = self.current_period();
        let current = period == self.period;
        let (period_committed, period_payments, period_denials) = if current {
            (
                self.period_committed,
                self.period_payments,
                self.period_denials,
            )
        } else {
            (0, 0, 0)
        };
        let period_start = self.start + period * self.policy.period_blocks;
        Summary {
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            refund_to: self.refund_to.clone(),
            policy: self.policy.clone(),
            committed: U128(self.committed),
            auto_committed: U128(self.auto_committed),
            held: U128(self.held),
            period,
            period_start,
            resets_at: period_start + self.policy.period_blocks,
            period_committed: U128(period_committed),
            period_payments,
            period_denials,
            exhausted: period_payments >= self.policy.max_payments,
            expired: self.expired(),
            payee_committed: self
                .payee_committed
                .iter()
                .map(|(payee, amount)| (payee.clone(), U128(*amount)))
                .collect(),
            attempts: self.attempts.len(),
            payments: self.payments,
            denials: self.denials,
            denials_not_stored: self.denials_not_stored,
            closed: self.closed,
        }
    }

    /// The request that reserved budget under this ID; denials are not indexed by ID.
    pub fn get_request(&self, request_id: String) -> Option<Attempt> {
        let index = *self.by_request_id.get(&request_id)?;
        Some(self.attempts[index].clone())
    }

    pub fn attempts(&self, from: u32, limit: u32) -> Vec<Attempt> {
        (from..self.attempts.len().min(from.saturating_add(limit)))
            .map(|index| self.attempts[index].clone())
            .collect()
    }
}

impl Mandate {
    /// `Ok(needs_approval)` if the request fits the policy, otherwise the denial reason.
    fn check(&self, request: &Request) -> Result<bool, Reason> {
        if let Some(index) = self.by_request_id.get(&request.request_id) {
            return Err(if self.attempts[*index].request == *request {
                Reason::Replay
            } else {
                Reason::Altered
            });
        }
        if request.policy_version != self.policy.version {
            return Err(Reason::PolicyVersion);
        }
        self.fits(request)?;
        if self.period_payments >= self.policy.max_payments {
            Err(Reason::Exhausted)
        } else {
            Ok(request.amount.0 > self.policy.approval_threshold.0 - self.period_committed)
        }
    }

    /// `Ok` if the payee is allowlisted and the amount fits its cap and the total cap now.
    fn fits(&self, request: &Request) -> Result<(), Reason> {
        let cap = self
            .policy
            .payees
            .get(&request.payee)
            .ok_or(Reason::Unlisted)?
            .0;
        let amount = request.amount.0;
        let payee_committed = self
            .payee_committed
            .get(&request.payee)
            .copied()
            .unwrap_or(0);
        if amount == 0 {
            Err(Reason::ZeroAmount)
        } else if amount > cap - payee_committed {
            Err(Reason::PayeeCap)
        } else if amount > self.policy.total_cap.0 - self.committed {
            Err(Reason::TotalCap)
        } else {
            Ok(())
        }
    }

    fn current_period(&self) -> u64 {
        (env::block_height() - self.start) / self.policy.period_blocks
    }

    /// Starts a new period, with nothing used in it, once its first block is reached.
    fn roll(&mut self) {
        let period = self.current_period();
        if period != self.period {
            self.period = period;
            self.period_committed = 0;
            self.period_payments = 0;
            self.period_denials = 0;
        }
    }

    fn expired(&self) -> bool {
        env::block_height() >= self.policy.expires_at
    }

    /// Charges a request about to be paid: before its transfer leaves.
    fn charge(&mut self, attempt: &Attempt) {
        let amount = attempt.request.amount.0;
        self.committed += amount;
        if !attempt.needs_approval {
            self.auto_committed += amount;
            self.period_committed += amount;
        }
        *self
            .payee_committed
            .entry(attempt.request.payee.clone())
            .or_default() += amount;
    }

    /// Gives a charge back. An agent-committed amount also frees its period's limit, if that
    /// period is still the current one.
    fn release(&mut self, attempt: &mut Attempt) {
        let amount = attempt.request.amount.0;
        self.committed -= amount;
        if !attempt.needs_approval {
            self.auto_committed -= amount;
            if attempt.period == self.period {
                self.period_committed -= amount;
            }
        }
        *self
            .payee_committed
            .get_mut(&attempt.request.payee)
            .unwrap() -= amount;
        attempt.releases += 1;
    }

    fn awaiting(&self, request_id: &str) -> (u32, Attempt) {
        require!(env::predecessor_account_id() == self.owner, "OWNER_ONLY");
        let index = *self
            .by_request_id
            .get(request_id)
            .unwrap_or_else(|| env::panic_str("UNKNOWN_REQUEST"));
        let attempt = self.attempts[index].clone();
        require!(
            attempt.status == Status::AwaitingApproval,
            "NOT_AWAITING_APPROVAL"
        );
        (index, attempt)
    }

    /// The transfer to the payee, then `on_transfer(index)` on this contract.
    fn pay(index: u32, attempt: &Attempt) -> PromiseIndex {
        let transfer = env::promise_batch_create(&attempt.request.payee);
        env::promise_batch_action_transfer(
            transfer,
            NearToken::from_yoctonear(attempt.request.amount.0),
        );
        let args = near_sdk::serde_json::json!({ "index": index }).to_string();
        env::promise_then(
            transfer,
            env::current_account_id(),
            "on_transfer",
            args,
            NearToken::from_yoctonear(0),
            CALLBACK_GAS,
        )
    }

    fn return_json(attempt: &Attempt) {
        env::value_return(near_sdk::serde_json::to_vec(attempt).unwrap_or_default());
    }

    fn event(event: &str, data: near_sdk::serde_json::Value) {
        env::log_str(&format!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({
                "standard": "preavis", "version": "3.0.0", "event": event, "data": [data]
            })
        ));
    }

    /// Emits the attempt as a NEP-297 event and stores it, unless it is a denial past the cap.
    fn save(&mut self, attempt: &Attempt) {
        let event = match attempt.status {
            Status::Denied => "denied",
            Status::AwaitingApproval => "awaiting_approval",
            Status::Pending => "pending",
            Status::Settled => "settled",
            Status::Failed => "failed",
            Status::Rejected => "rejected",
        };
        Self::event(event, near_sdk::serde_json::json!(attempt));
        match attempt.index {
            Some(index) if index == self.attempts.len() => self.attempts.push(attempt.clone()),
            Some(index) => {
                self.attempts.replace(index, attempt.clone());
            }
            None => {}
        }
    }
}
