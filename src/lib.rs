//! KeyDare: a spending mandate for an AI agent on NEAR.
//!
//! The mandate account holds the budget. The agent may only call `request_spend`; every request is
//! checked against a fixed policy, charged before the transfer leaves, and settled or released by a
//! private callback. The owner can only approve or reject requests held above the approval threshold.
//! There is no method that deploys code, adds keys or edits the policy.

use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue, env, near,
    require,
};
use std::collections::BTreeMap;

const MAX_REQUEST_ID_LEN: usize = 64;
const CALLBACK_GAS: Gas = Gas::from_tgas(15);

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
}

/// Everything a request is bound to. A request ID can be used once; resubmitting it is a replay
/// if every field matches and an altered request otherwise.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub request_id: String,
    pub actor: AccountId,
    pub payee: AccountId,
    pub amount: U128,
    pub policy_version: u32,
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
}

/// One call to `request_spend`, denied or not. Attempts are append-only.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Attempt {
    pub index: u32,
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
    pub policy: Policy,
    pub committed: U128,
    pub auto_committed: U128,
    pub payee_committed: BTreeMap<AccountId, U128>,
    pub attempts: u32,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Mandate {
    owner: AccountId,
    agent: AccountId,
    policy: Policy,
    /// Held, in flight and settled amounts.
    committed: u128,
    /// The part of `committed` that did not need owner approval.
    auto_committed: u128,
    payee_committed: BTreeMap<AccountId, u128>,
    attempts: Vector<Attempt>,
    by_request_id: LookupMap<String, u32>,
}

#[near]
impl Mandate {
    #[init]
    #[private]
    pub fn new(owner: AccountId, agent: AccountId, policy: Policy) -> Self {
        require!(owner != agent, "OWNER_IS_AGENT");
        require!(
            policy.approval_threshold.0 <= policy.total_cap.0,
            "THRESHOLD_ABOVE_TOTAL"
        );
        Self {
            owner,
            agent,
            policy,
            committed: 0,
            auto_committed: 0,
            payee_committed: BTreeMap::new(),
            attempts: Vector::new(b"a"),
            by_request_id: LookupMap::new(b"r"),
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
    ) -> PromiseOrValue<Attempt> {
        let actor = env::predecessor_account_id();
        require!(actor == self.agent, "AGENT_ONLY");
        require!(
            !request_id.is_empty() && request_id.len() <= MAX_REQUEST_ID_LEN,
            "BAD_REQUEST_ID"
        );
        let request = Request {
            request_id,
            actor,
            payee,
            amount,
            policy_version,
        };
        let index = self.attempts.len();
        let checked = self.check(&request);
        if !matches!(checked, Err(Reason::Replay | Reason::Altered)) {
            self.by_request_id.insert(request.request_id.clone(), index);
        }
        let attempt = match checked {
            Err(reason) => Attempt {
                index,
                request,
                status: Status::Denied,
                reason: Some(reason),
                needs_approval: false,
                releases: 0,
            },
            Ok(needs_approval) => {
                self.charge(&request.payee, request.amount.0, needs_approval);
                Attempt {
                    index,
                    request,
                    status: if needs_approval {
                        Status::AwaitingApproval
                    } else {
                        Status::Pending
                    },
                    reason: None,
                    needs_approval,
                    releases: 0,
                }
            }
        };
        self.save(attempt.clone());
        if attempt.status == Status::Pending {
            PromiseOrValue::Promise(Self::pay(&attempt))
        } else {
            PromiseOrValue::Value(attempt)
        }
    }

    /// Owner only. Every bound field must match the held request exactly.
    pub fn approve(
        &mut self,
        request_id: String,
        payee: AccountId,
        amount: U128,
        policy_version: u32,
    ) -> Promise {
        let mut attempt = self.awaiting(&request_id);
        let bound = Request {
            request_id,
            actor: self.agent.clone(),
            payee,
            amount,
            policy_version,
        };
        require!(attempt.request == bound, "BINDING_MISMATCH");
        attempt.status = Status::Pending;
        self.save(attempt.clone());
        Self::pay(&attempt)
    }

    /// Owner only. Releases the held charge.
    pub fn reject(&mut self, request_id: String) -> Attempt {
        let mut attempt = self.awaiting(&request_id);
        self.release(&mut attempt);
        attempt.status = Status::Rejected;
        self.save(attempt.clone());
        attempt
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
        self.save(attempt.clone());
        attempt
    }

    pub fn mandate(&self) -> Summary {
        Summary {
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            policy: self.policy.clone(),
            committed: U128(self.committed),
            auto_committed: U128(self.auto_committed),
            payee_committed: self
                .payee_committed
                .iter()
                .map(|(payee, amount)| (payee.clone(), U128(*amount)))
                .collect(),
            attempts: self.attempts.len(),
        }
    }

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
        } else {
            Ok(amount > self.policy.approval_threshold.0 - self.auto_committed)
        }
    }

    fn charge(&mut self, payee: &AccountId, amount: u128, needs_approval: bool) {
        self.committed += amount;
        if !needs_approval {
            self.auto_committed += amount;
        }
        *self.payee_committed.entry(payee.clone()).or_default() += amount;
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

    fn awaiting(&self, request_id: &str) -> Attempt {
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
        attempt
    }

    fn pay(attempt: &Attempt) -> Promise {
        Promise::new(attempt.request.payee.clone())
            .transfer(NearToken::from_yoctonear(attempt.request.amount.0))
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(CALLBACK_GAS)
                    .on_transfer(attempt.index),
            )
    }

    /// Stores the attempt and emits it as a NEP-297 event.
    fn save(&mut self, attempt: Attempt) {
        let event = match attempt.status {
            Status::Denied => "denied",
            Status::AwaitingApproval => "awaiting_approval",
            Status::Pending => "pending",
            Status::Settled => "settled",
            Status::Failed => "failed",
            Status::Rejected => "rejected",
        };
        env::log_str(&format!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({
                "standard": "keydare", "version": "1.0.0", "event": event, "data": [&attempt]
            })
        ));
        if attempt.index == self.attempts.len() {
            self.attempts.push(attempt);
        } else {
            self.attempts.replace(attempt.index, attempt);
        }
    }
}
