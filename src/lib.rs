//! Preavis: a spending mandate for an AI agent on NEAR.
//!
//! The mandate account holds the budget. The agent may only call `request_spend`; every request is
//! checked against a fixed policy, charged before the transfer leaves, and settled or released by a
//! private callback. The owner can only approve or reject requests held above the approval threshold,
//! and close the mandate, which sends what is left to the `refund_to` account fixed at init.
//! There is no method that deploys code, adds keys or edits the policy.

use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue, env, near,
    require,
};
use std::collections::BTreeMap;

/// Longest request ID or invoice reference, in bytes.
const MAX_ID_LEN: usize = 64;
const CALLBACK_GAS: Gas = Gas::from_tgas(15);
/// Upper bounds on storage, in bytes, with every ID and account name at its 64-byte maximum and
/// NEAR's 40-byte overhead per stored entry. One stored attempt: its record (347) and, for a
/// payment, its request-ID entry (113).
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
    /// The most the mandate may ever commit, owner-approved requests included.
    pub total_cap: U128,
    /// The agent may commit this much on its own; a request beyond it waits for the owner.
    pub approval_threshold: U128,
    /// How many requests may ever reserve budget (paid, held or failed). Bounds storage.
    pub max_payments: u32,
    /// How many denials are stored; later ones are only counted and emitted as events.
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
    PaymentLimit,
}

/// One call to `request_spend`, denied or not.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Attempt {
    /// Position in `attempts`; null for a denial past `max_denials`, which is not stored.
    pub index: Option<u32>,
    #[serde(flatten)]
    pub request: Request,
    pub status: Status,
    pub reason: Option<Reason>,
    pub needs_approval: bool,
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
    pub held: U128,
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
    /// Held, in flight and settled amounts.
    committed: u128,
    /// The part of `committed` that did not need owner approval.
    auto_committed: u128,
    /// The part of `committed` waiting for the owner: still in the account.
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
    /// The balance must cover the total cap plus storage for every record the policy allows, so
    /// requests can always be recorded and settled.
    #[init]
    #[private]
    pub fn new(owner: AccountId, agent: AccountId, refund_to: AccountId, policy: Policy) -> Self {
        require!(owner != agent, "OWNER_IS_AGENT");
        require!(
            policy.approval_threshold.0 <= policy.total_cap.0,
            "THRESHOLD_ABOVE_TOTAL"
        );
        let bytes = env::storage_usage()
            + STATE_BYTES
            + PAYEE_BYTES * policy.payees.len() as u64
            + RECORD_BYTES * (u64::from(policy.max_payments) + u64::from(policy.max_denials));
        let reserve = env::storage_byte_cost().as_yoctonear() * u128::from(bytes);
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
    /// either paid at once or held for the owner.
    pub fn request_spend(
        &mut self,
        request_id: String,
        payee: AccountId,
        amount: U128,
        policy_version: u32,
        invoice: String,
    ) -> PromiseOrValue<Attempt> {
        let actor = env::predecessor_account_id();
        require!(actor == self.agent, "AGENT_ONLY");
        require!(!self.closed, "CLOSED");
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
            releases: 0,
        };
        match self.check(&attempt.request) {
            Err(reason) => {
                attempt.reason = Some(reason);
                if self.denials < self.policy.max_denials {
                    self.denials += 1;
                    attempt.index = Some(self.attempts.len());
                } else {
                    self.denials_not_stored += 1;
                }
            }
            Ok(needs_approval) => {
                let index = self.attempts.len();
                self.charge(&attempt.request, needs_approval);
                self.payments += 1;
                self.by_request_id
                    .insert(attempt.request.request_id.clone(), index);
                attempt.index = Some(index);
                attempt.needs_approval = needs_approval;
                attempt.status = if needs_approval {
                    Status::AwaitingApproval
                } else {
                    Status::Pending
                };
            }
        }
        self.save(&attempt);
        match (attempt.status, attempt.index) {
            (Status::Pending, Some(index)) => PromiseOrValue::Promise(Self::pay(index, &attempt)),
            _ => PromiseOrValue::Value(attempt),
        }
    }

    /// Owner only. Every bound field, the invoice included, must match the held request exactly.
    pub fn approve(
        &mut self,
        request_id: String,
        payee: AccountId,
        amount: U128,
        policy_version: u32,
        invoice: String,
    ) -> Promise {
        let (index, mut attempt) = self.awaiting(&request_id);
        let bound = Request {
            request_id,
            actor: self.agent.clone(),
            payee,
            amount,
            policy_version,
            invoice,
        };
        require!(attempt.request == bound, "BINDING_MISMATCH");
        self.held -= attempt.request.amount.0;
        attempt.status = Status::Pending;
        self.save(&attempt);
        Self::pay(index, &attempt)
    }

    /// Owner only. Releases the held charge.
    pub fn reject(&mut self, request_id: String) -> Attempt {
        let (_, mut attempt) = self.awaiting(&request_id);
        self.held -= attempt.request.amount.0;
        self.release(&mut attempt);
        attempt.status = Status::Rejected;
        self.save(&attempt);
        attempt
    }

    /// Owner only. Ends the mandate: from now on the agent's requests are refused. Held requests
    /// can still be approved or rejected. Everything else except the storage stake goes to
    /// `refund_to`, fixed at init; call again to send anything that arrives later.
    pub fn close(&mut self) -> Promise {
        require!(env::predecessor_account_id() == self.owner, "OWNER_ONLY");
        self.closed = true;
        let keep = env::storage_byte_cost().as_yoctonear() * u128::from(env::storage_usage())
            + self.held;
        let amount = env::account_balance().as_yoctonear().saturating_sub(keep);
        Self::event(
            "closed",
            near_sdk::serde_json::json!({"refund_to": &self.refund_to, "amount": U128(amount)}),
        );
        Promise::new(self.refund_to.clone()).transfer(NearToken::from_yoctonear(amount))
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
        Summary {
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            refund_to: self.refund_to.clone(),
            policy: self.policy.clone(),
            committed: U128(self.committed),
            auto_committed: U128(self.auto_committed),
            held: U128(self.held),
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
        let cap = self
            .policy
            .payees
            .get(&request.payee)
            .ok_or(Reason::Unlisted)?
            .0;
        let amount = request.amount.0;
        let payee_committed = self.payee_committed.get(&request.payee).copied().unwrap_or(0);
        if amount == 0 {
            Err(Reason::ZeroAmount)
        } else if amount > cap - payee_committed {
            Err(Reason::PayeeCap)
        } else if amount > self.policy.total_cap.0 - self.committed {
            Err(Reason::TotalCap)
        } else if self.payments >= self.policy.max_payments {
            Err(Reason::PaymentLimit)
        } else {
            Ok(amount > self.policy.approval_threshold.0 - self.auto_committed)
        }
    }

    fn charge(&mut self, request: &Request, needs_approval: bool) {
        let amount = request.amount.0;
        self.committed += amount;
        if needs_approval {
            self.held += amount;
        } else {
            self.auto_committed += amount;
        }
        *self.payee_committed.entry(request.payee.clone()).or_default() += amount;
    }

    fn release(&mut self, attempt: &mut Attempt) {
        let amount = attempt.request.amount.0;
        self.committed -= amount;
        if !attempt.needs_approval {
            self.auto_committed -= amount;
        }
        *self.payee_committed.get_mut(&attempt.request.payee).unwrap() -= amount;
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

    fn pay(index: u32, attempt: &Attempt) -> Promise {
        Promise::new(attempt.request.payee.clone())
            .transfer(NearToken::from_yoctonear(attempt.request.amount.0))
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(CALLBACK_GAS)
                    .on_transfer(index),
            )
    }

    fn event(event: &str, data: near_sdk::serde_json::Value) {
        env::log_str(&format!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({
                "standard": "preavis", "version": "2.0.0", "event": event, "data": [data]
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
