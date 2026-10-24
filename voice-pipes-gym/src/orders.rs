//! Native in-memory order logistics & customer support task environment.
//!
//! Models tracking orders, changing shipping addresses, and cancellations
//! with alphanumeric phonetic spelling challenges and instant rollback.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use voice_pipes_sdk::agent::{ClientToolCall, ClientToolSpec};

use crate::env::{TaskEnvironment, TaskEvaluation};

/// Record for a customer order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderRecord {
    pub order_id: String,
    pub customer_name: String,
    pub item: String,
    pub shipping_address: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
struct OrdersSnapshot {
    orders: BTreeMap<String, OrderRecord>,
}

/// Verifiable goal for an order support dialogue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OrderGoal {
    /// Order shipping address was updated to exact expected address.
    AddressUpdated {
        order_id: String,
        expected_address: String,
    },
    /// Order status was set to cancelled.
    OrderCancelled { order_id: String },
    /// Multiple required goals.
    AllOf(Vec<OrderGoal>),
}

/// In-memory order management environment.
#[derive(Debug, Clone)]
pub struct OrderSupportTaskEnv {
    orders: BTreeMap<String, OrderRecord>,
    snapshot: OrdersSnapshot,
}

impl Default for OrderSupportTaskEnv {
    fn default() -> Self {
        Self::new_default()
    }
}

impl OrderSupportTaskEnv {
    /// Create standard scenario with default orders.
    pub fn new_default() -> Self {
        let mut orders = BTreeMap::new();
        orders.insert(
            "B482".to_string(),
            OrderRecord {
                order_id: "B482".to_string(),
                customer_name: "Alice Martin".to_string(),
                item: "Wireless Headphones".to_string(),
                shipping_address: "123 Main St, Springfield".to_string(),
                status: "processing".to_string(),
            },
        );
        orders.insert(
            "K901".to_string(),
            OrderRecord {
                order_id: "K901".to_string(),
                customer_name: "Bob Vance".to_string(),
                item: "Espresso Machine".to_string(),
                shipping_address: "456 Oak Rd, Scranton".to_string(),
                status: "shipped".to_string(),
            },
        );

        let snapshot = OrdersSnapshot {
            orders: orders.clone(),
        };

        Self { orders, snapshot }
    }

    /// Retrieve an order by normalized ID.
    pub fn get_order(&self, order_id: &str) -> Option<&OrderRecord> {
        let normalized = order_id.trim().to_ascii_uppercase();
        self.orders.get(&normalized)
    }

    /// Evaluate current in-memory state against the scenario goal.
    pub fn verify_goal(&self, goal: &OrderGoal) -> TaskEvaluation {
        match goal {
            OrderGoal::AddressUpdated {
                order_id,
                expected_address,
            } => match self.get_order(order_id) {
                Some(order) => {
                    let norm_actual = order.shipping_address.trim().to_ascii_lowercase();
                    let norm_expected = expected_address.trim().to_ascii_lowercase();
                    if norm_actual == norm_expected {
                        TaskEvaluation::success(format!(
                            "Order {}: shipping address successfully updated to '{}'",
                            order.order_id, order.shipping_address
                        ))
                    } else {
                        TaskEvaluation::failure(format!(
                            "Order {}: address was updated to '{}' instead of expected '{}'",
                            order.order_id, order.shipping_address, expected_address
                        ))
                    }
                }
                None => TaskEvaluation::failure(format!("Order {order_id} not found")),
            },

            OrderGoal::OrderCancelled { order_id } => match self.get_order(order_id) {
                Some(order) => {
                    if order.status == "cancelled" {
                        TaskEvaluation::success(format!("Order {order_id} successfully cancelled"))
                    } else {
                        TaskEvaluation::failure(format!(
                            "Order {order_id} status is '{}', expected 'cancelled'",
                            order.status
                        ))
                    }
                }
                None => TaskEvaluation::failure(format!("Order {order_id} not found")),
            },

            OrderGoal::AllOf(subgoals) => {
                let mut total_score = 0.0;
                let mut messages = Vec::new();
                let count = subgoals.len();
                if count == 0 {
                    return TaskEvaluation::success("Empty goal satisfied");
                }
                for sub in subgoals {
                    let eval = self.verify_goal(sub);
                    total_score += eval.score;
                    messages.push(eval.message);
                }
                let final_score = total_score / (count as f32);
                TaskEvaluation {
                    success: final_score >= 0.99,
                    score: final_score,
                    message: messages.join("; "),
                }
            }
        }
    }

    fn execute_lookup(&self, args: &Value) -> Result<Value, String> {
        let raw_id = args
            .get("order_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'order_id'".to_string())?;

        let order = self
            .get_order(raw_id)
            .ok_or_else(|| format!("Order '{raw_id}' not found in system"))?;

        Ok(json!({
            "order_id": order.order_id,
            "customer_name": order.customer_name,
            "item": order.item,
            "shipping_address": order.shipping_address,
            "status": order.status
        }))
    }

    fn execute_update_address(&mut self, args: &Value) -> Result<Value, String> {
        let raw_id = args
            .get("order_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'order_id'".to_string())?;

        let new_address = args
            .get("new_address")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'new_address'".to_string())?;

        let normalized_id = raw_id.trim().to_ascii_uppercase();
        let order = self
            .orders
            .get_mut(&normalized_id)
            .ok_or_else(|| format!("Order '{raw_id}' not found"))?;

        if order.status == "shipped" || order.status == "delivered" {
            return Err(format!(
                "Cannot change address: order '{normalized_id}' is already {}",
                order.status
            ));
        }

        order.shipping_address = new_address.trim().to_string();

        Ok(json!({
            "status": "success",
            "order_id": normalized_id,
            "updated_shipping_address": order.shipping_address
        }))
    }

    fn execute_cancel(&mut self, args: &Value) -> Result<Value, String> {
        let raw_id = args
            .get("order_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'order_id'".to_string())?;

        let normalized_id = raw_id.trim().to_ascii_uppercase();
        let order = self
            .orders
            .get_mut(&normalized_id)
            .ok_or_else(|| format!("Order '{raw_id}' not found"))?;

        if order.status == "shipped" || order.status == "delivered" {
            return Err(format!(
                "Cannot cancel order: package is already {}",
                order.status
            ));
        }

        order.status = "cancelled".to_string();

        Ok(json!({
            "status": "success",
            "order_id": normalized_id,
            "order_status": "cancelled"
        }))
    }
}

impl TaskEnvironment for OrderSupportTaskEnv {
    fn name(&self) -> &'static str {
        "order_support"
    }

    fn tools(&self) -> Vec<ClientToolSpec> {
        vec![
            ClientToolSpec::new(
                "lookup_order",
                "Look up tracking information and current shipping address for an order ID.",
                json!({
                    "type": "object",
                    "properties": {
                        "order_id": {
                            "type": "string",
                            "description": "Alphanumeric order identifier (e.g. 'B482')"
                        }
                    },
                    "required": ["order_id"]
                }),
            ),
            ClientToolSpec::new(
                "update_shipping_address",
                "Update the delivery address for an order that has not yet shipped.",
                json!({
                    "type": "object",
                    "properties": {
                        "order_id": {
                            "type": "string",
                            "description": "Order ID"
                        },
                        "new_address": {
                            "type": "string",
                            "description": "Complete new delivery address"
                        }
                    },
                    "required": ["order_id", "new_address"]
                }),
            ),
            ClientToolSpec::new(
                "cancel_order",
                "Cancel an active pending order prior to carrier dispatch.",
                json!({
                    "type": "object",
                    "properties": {
                        "order_id": {
                            "type": "string",
                            "description": "Order ID to cancel"
                        },
                        "reason": {
                            "type": "string",
                            "description": "Customer provided reason for cancellation"
                        }
                    },
                    "required": ["order_id"]
                }),
            ),
        ]
    }

    fn execute(&mut self, call: &ClientToolCall) -> Result<Value, String> {
        match call.name.as_str() {
            "lookup_order" => self.execute_lookup(&call.arguments),
            "update_shipping_address" => self.execute_update_address(&call.arguments),
            "cancel_order" => self.execute_cancel(&call.arguments),
            unknown => Err(format!("Unknown order support tool '{unknown}'")),
        }
    }

    fn reset(&mut self) {
        self.orders = self.snapshot.orders.clone();
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

    #[test]
    fn test_order_lookup_and_update() {
        let mut env = OrderSupportTaskEnv::new_default();

        let lookup = ClientToolCall::new("c1", "lookup_order", json!({"order_id": "b482"}));
        let res = env.execute(&lookup).expect("lookup succeeded");
        assert_eq!(res["order_id"], "B482");
        assert_eq!(res["status"], "processing");

        let update = ClientToolCall::new(
            "c2",
            "update_shipping_address",
            json!({"order_id": "B482", "new_address": "789 Pine Ave, Seattle"}),
        );
        let update_res = env.execute(&update).expect("update succeeded");
        assert_eq!(update_res["status"], "success");

        let goal = OrderGoal::AddressUpdated {
            order_id: "B482".to_string(),
            expected_address: "789 Pine Ave, Seattle".to_string(),
        };
        let eval = env.verify_goal(&goal);
        assert!(eval.success);
    }

    #[test]
    fn test_cannot_update_shipped_order() {
        let mut env = OrderSupportTaskEnv::new_default();

        let update = ClientToolCall::new(
            "c3",
            "update_shipping_address",
            json!({"order_id": "K901", "new_address": "999 New Road"}),
        );
        let err = env.execute(&update).unwrap_err();
        assert!(err.contains("already shipped"));
    }
}
