//! Native in-memory banking task environment for voice agent testing.
//!
//! Models checking/savings accounts, card security, and fund transfers with
//! microsecond rollbacks, zero external dependencies, and deterministic
//! state verification.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use voice_pipes_sdk::agent::{ClientToolCall, ClientToolSpec};

use crate::env::{TaskEnvironment, TaskEvaluation};

/// Transaction log entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransactionRecord {
    pub transaction_id: usize,
    pub from_account: String,
    pub to_account: String,
    pub amount: f64,
}

/// Snapshot of the banking state for microsecond resets.
#[derive(Debug, Clone, PartialEq)]
struct BankingSnapshot {
    accounts: BTreeMap<String, f64>,
    locked_cards: BTreeSet<String>,
}

/// The verifiable goal or expected outcome of a banking dialogue scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BankingGoal {
    /// A single fund transfer with exact amount.
    Transfer {
        from_account: String,
        to_account: String,
        expected_amount: f64,
    },
    /// A card lock action.
    LockCard { card_last4: String },
    /// Multiple required goals within a multi-turn conversation.
    AllOf(Vec<BankingGoal>),
}

/// In-memory banking environment.
#[derive(Debug, Clone)]
pub struct BankingTaskEnv {
    accounts: BTreeMap<String, f64>,
    known_cards: BTreeSet<String>,
    locked_cards: BTreeSet<String>,
    transactions: Vec<TransactionRecord>,
    next_tx_id: usize,
    snapshot: BankingSnapshot,
}

impl Default for BankingTaskEnv {
    fn default() -> Self {
        Self::new_default()
    }
}

impl BankingTaskEnv {
    /// Create a standard phone banking scenario with default account balances.
    pub fn new_default() -> Self {
        let mut accounts = BTreeMap::new();
        accounts.insert("checking".to_string(), 1250.00);
        accounts.insert("savings".to_string(), 5400.00);
        accounts.insert("credit".to_string(), 420.00);

        let mut known_cards = BTreeSet::new();
        known_cards.insert("4821".to_string());
        known_cards.insert("9012".to_string());

        let snapshot = BankingSnapshot {
            accounts: accounts.clone(),
            locked_cards: BTreeSet::new(),
        };

        Self {
            accounts,
            known_cards,
            locked_cards: BTreeSet::new(),
            transactions: Vec::new(),
            next_tx_id: 1,
            snapshot,
        }
    }

    /// Create a custom banking environment with specified accounts and cards.
    pub fn new(accounts: BTreeMap<String, f64>, known_cards: BTreeSet<String>) -> Self {
        let snapshot = BankingSnapshot {
            accounts: accounts.clone(),
            locked_cards: BTreeSet::new(),
        };
        Self {
            accounts,
            known_cards,
            locked_cards: BTreeSet::new(),
            transactions: Vec::new(),
            next_tx_id: 1,
            snapshot,
        }
    }

    /// Read an account balance.
    pub fn get_balance(&self, account: &str) -> Option<f64> {
        self.accounts.get(account).copied()
    }

    /// Check if a card is locked.
    pub fn is_card_locked(&self, card_last4: &str) -> bool {
        self.locked_cards.contains(card_last4)
    }

    /// List completed transactions.
    pub fn transactions(&self) -> &[TransactionRecord] {
        &self.transactions
    }

    /// Evaluate current in-memory state against the scenario goal.
    pub fn verify_goal(&self, goal: &BankingGoal) -> TaskEvaluation {
        match goal {
            BankingGoal::Transfer {
                from_account,
                to_account,
                expected_amount,
            } => {
                let matching_tx = self.transactions.iter().find(|tx| {
                    tx.from_account == *from_account
                        && tx.to_account == *to_account
                        && (tx.amount - *expected_amount).abs() < 1e-4
                });

                if let Some(tx) = matching_tx {
                    // Check if extra unintended transfers occurred
                    if self.transactions.len() == 1 {
                        TaskEvaluation::success(format!(
                            "Transfer #{}: successfully transferred ${:.2} from {} to {}",
                            tx.transaction_id, tx.amount, tx.from_account, tx.to_account
                        ))
                    } else {
                        TaskEvaluation::partial(
                            0.7,
                            format!(
                                "Correct transfer of ${:.2} found, but {} extra transactions were executed",
                                expected_amount,
                                self.transactions.len().saturating_sub(1)
                            ),
                        )
                    }
                } else {
                    // Diagnostic check: did they transfer a wrong amount (e.g. speech correction missed)?
                    let wrong_amount_tx = self.transactions.iter().find(|tx| {
                        tx.from_account == *from_account && tx.to_account == *to_account
                    });
                    if let Some(tx) = wrong_amount_tx {
                        TaskEvaluation::failure(format!(
                            "Transfer executed with incorrect amount: ${:.2} instead of ${:.2} (likely missed speech self-correction)",
                            tx.amount, expected_amount
                        ))
                    } else if self.transactions.is_empty() {
                        TaskEvaluation::failure("No transfer was executed by the agent")
                    } else {
                        TaskEvaluation::failure("Transfer executed between incorrect accounts")
                    }
                }
            }

            BankingGoal::LockCard { card_last4 } => {
                if self.is_card_locked(card_last4) {
                    TaskEvaluation::success(format!("Card ending in {card_last4} is locked"))
                } else {
                    TaskEvaluation::failure(format!(
                        "Card ending in {card_last4} was not locked by the agent"
                    ))
                }
            }

            BankingGoal::AllOf(subgoals) => {
                let mut total_score = 0.0;
                let mut diagnostics = Vec::new();
                let count = subgoals.len();
                if count == 0 {
                    return TaskEvaluation::success("Empty goal satisfied");
                }

                for subgoal in subgoals {
                    let eval = self.verify_goal(subgoal);
                    total_score += eval.score;
                    diagnostics.push(eval.message);
                }

                let final_score = total_score / (count as f32);
                TaskEvaluation {
                    success: final_score >= 0.99,
                    score: final_score,
                    message: diagnostics.join("; "),
                }
            }
        }
    }

    fn execute_get_balance(&self, args: &Value) -> Result<Value, String> {
        let account = args
            .get("account")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'account'".to_string())?;

        let balance = self
            .get_balance(account)
            .ok_or_else(|| format!("Unknown account '{account}'"))?;

        Ok(json!({
            "account": account,
            "balance": balance,
            "currency": "USD"
        }))
    }

    fn execute_transfer(&mut self, args: &Value) -> Result<Value, String> {
        let from = args
            .get("from_account")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'from_account'".to_string())?;

        let to = args
            .get("to_account")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'to_account'".to_string())?;

        let amount = args
            .get("amount")
            .and_then(Value::as_f64)
            .ok_or_else(|| "Missing or invalid 'amount'".to_string())?;

        if amount <= 0.0 {
            return Err("Transfer amount must be greater than zero".to_string());
        }

        if from == to {
            return Err("Source and destination accounts must be different".to_string());
        }

        let from_bal = self
            .get_balance(from)
            .ok_or_else(|| format!("Unknown source account '{from}'"))?;

        let to_bal = self
            .get_balance(to)
            .ok_or_else(|| format!("Unknown destination account '{to}'"))?;

        if from_bal < amount {
            return Err(format!(
                "Insufficient funds in '{from}': balance ${from_bal:.2} is less than ${amount:.2}"
            ));
        }

        let new_from = from_bal - amount;
        let new_to = to_bal + amount;

        self.accounts.insert(from.to_string(), new_from);
        self.accounts.insert(to.to_string(), new_to);

        let tx_id = self.next_tx_id;
        self.next_tx_id = self.next_tx_id.saturating_add(1);

        self.transactions.push(TransactionRecord {
            transaction_id: tx_id,
            from_account: from.to_string(),
            to_account: to.to_string(),
            amount,
        });

        Ok(json!({
            "status": "success",
            "transaction_id": tx_id,
            "from_account": from,
            "to_account": to,
            "amount": amount,
            "from_new_balance": new_from,
            "to_new_balance": new_to
        }))
    }

    fn execute_lock_card(&mut self, args: &Value) -> Result<Value, String> {
        let card_last4 = args
            .get("card_last4")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'card_last4'".to_string())?;

        if !self.known_cards.contains(card_last4) {
            return Err(format!("Card ending in '{card_last4}' not found on file"));
        }

        self.locked_cards.insert(card_last4.to_string());

        Ok(json!({
            "status": "locked",
            "card_last4": card_last4,
            "message": format!("Card ending in {card_last4} has been locked")
        }))
    }
}

impl TaskEnvironment for BankingTaskEnv {
    fn name(&self) -> &'static str {
        "banking"
    }

    fn tools(&self) -> Vec<ClientToolSpec> {
        vec![
            ClientToolSpec::new(
                "get_balance",
                "Look up current available balance for a named bank account.",
                json!({
                    "type": "object",
                    "properties": {
                        "account": {
                            "type": "string",
                            "enum": ["checking", "savings", "credit"],
                            "description": "Name of the account to check"
                        }
                    },
                    "required": ["account"]
                }),
            ),
            ClientToolSpec::new(
                "transfer_funds",
                "Transfer funds between two bank accounts.",
                json!({
                    "type": "object",
                    "properties": {
                        "from_account": {
                            "type": "string",
                            "description": "Source account (e.g. 'checking' or 'savings')"
                        },
                        "to_account": {
                            "type": "string",
                            "description": "Destination account"
                        },
                        "amount": {
                            "type": "number",
                            "minimum": 0.01,
                            "description": "Dollar amount to transfer"
                        }
                    },
                    "required": ["from_account", "to_account", "amount"]
                }),
            ),
            ClientToolSpec::new(
                "lock_card",
                "Immediately lock a debit or credit card to prevent unauthorized charges.",
                json!({
                    "type": "object",
                    "properties": {
                        "card_last4": {
                            "type": "string",
                            "description": "Last 4 digits of the card number to lock"
                        }
                    },
                    "required": ["card_last4"]
                }),
            ),
        ]
    }

    fn execute(&mut self, call: &ClientToolCall) -> Result<Value, String> {
        let args = &call.arguments;

        match call.name.as_str() {
            "get_balance" => self.execute_get_balance(args),
            "transfer_funds" => self.execute_transfer(args),
            "lock_card" => self.execute_lock_card(args),
            unknown => Err(format!("Unknown banking tool '{unknown}'")),
        }
    }

    fn reset(&mut self) {
        self.accounts = self.snapshot.accounts.clone();
        self.locked_cards = self.snapshot.locked_cards.clone();
        self.transactions.clear();
        self.next_tx_id = 1;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_banking_tools_declared() {
        let env = BankingTaskEnv::new_default();
        let tools = env.tools();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"get_balance"));
        assert!(names.contains(&"transfer_funds"));
        assert!(names.contains(&"lock_card"));
    }

    #[test]
    fn test_get_balance_execution() {
        let mut env = BankingTaskEnv::new_default();
        let call = ClientToolCall::new("c1", "get_balance", json!({"account": "checking"}));
        let res = env.execute(&call).expect("success");
        assert_eq!(res["balance"], 1250.0);
    }

    #[test]
    fn test_transfer_success_and_goal_verification() {
        let mut env = BankingTaskEnv::new_default();
        let call = ClientToolCall::new(
            "c2",
            "transfer_funds",
            json!({"from_account": "checking", "to_account": "savings", "amount": 250.0}),
        );

        let res = env.execute(&call).expect("transfer succeeded");
        assert_eq!(res["status"], "success");
        assert_eq!(env.get_balance("checking"), Some(1000.0));
        assert_eq!(env.get_balance("savings"), Some(5650.0));

        let goal = BankingGoal::Transfer {
            from_account: "checking".to_string(),
            to_account: "savings".to_string(),
            expected_amount: 250.0,
        };
        let eval = env.verify_goal(&goal);
        assert!(eval.success);
        assert!((eval.score - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_insufficient_funds() {
        let mut env = BankingTaskEnv::new_default();
        let call = ClientToolCall::new(
            "c3",
            "transfer_funds",
            json!({"from_account": "checking", "to_account": "savings", "amount": 99999.0}),
        );
        let err = env.execute(&call).unwrap_err();
        assert!(err.contains("Insufficient funds"));
    }

    #[test]
    fn test_lock_card_and_verification() {
        let mut env = BankingTaskEnv::new_default();
        assert!(!env.is_card_locked("4821"));

        let call = ClientToolCall::new("c4", "lock_card", json!({"card_last4": "4821"}));
        let res = env.execute(&call).expect("card locked");
        assert_eq!(res["status"], "locked");
        assert!(env.is_card_locked("4821"));

        let goal = BankingGoal::LockCard {
            card_last4: "4821".to_string(),
        };
        let eval = env.verify_goal(&goal);
        assert!(eval.success);
    }

    #[test]
    fn test_microsecond_reset_latency() {
        let mut env = BankingTaskEnv::new_default();

        // Mutate state with several actions
        let call_transfer = ClientToolCall::new(
            "c1",
            "transfer_funds",
            json!({"from_account": "checking", "to_account": "savings", "amount": 100.0}),
        );
        let call_lock = ClientToolCall::new("c2", "lock_card", json!({"card_last4": "4821"}));

        env.execute(&call_transfer).unwrap();
        env.execute(&call_lock).unwrap();

        assert_eq!(env.get_balance("checking"), Some(1150.0));
        assert!(env.is_card_locked("4821"));

        // Measure reset time over 1,000 iterations
        let start = Instant::now();
        for _ in 0..1000 {
            env.reset();
        }
        let elapsed = start.elapsed();

        // State must be completely restored
        assert_eq!(env.get_balance("checking"), Some(1250.0));
        assert!(!env.is_card_locked("4821"));
        assert!(env.transactions().is_empty());

        // 1,000 in-memory resets must take < 5ms (averaging < 5µs per reset)
        assert!(
            elapsed.as_millis() < 5,
            "1,000 resets took {:?} (expected < 5ms)",
            elapsed
        );
    }
}
