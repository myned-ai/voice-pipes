//! Integration tests for `GymTask` scenarios, acoustic adversary, and composite rewards.

use std::time::Instant;

use voice_pipes_bench::types::{
    GymResult, GymTaskConfig, ScenarioInput, ScenarioMetrics, ScenarioResult, ScenarioRunStatus,
    TestScenario, TestSuite,
};
use voice_pipes_gym::{
    AcousticAdversary, AdversaryConfig, BankingGoal, BankingTaskEnv, ClientToolCall,
    ClinicBookingTaskEnv, ClinicGoal, GymRewardBreakdown, GymRewardWeights, OrderGoal,
    OrderSupportTaskEnv, TaskEnvironment,
};

#[test]
fn test_gym_task_scenario_round_trip_serialization() {
    let goal_json = serde_json::to_value(BankingGoal::Transfer {
        from_account: "checking".to_string(),
        to_account: "savings".to_string(),
        expected_amount: 250.0,
    })
    .expect("serialize goal");

    let gym_task = GymTaskConfig {
        domain: "banking".to_string(),
        goal: goal_json,
        initial_prompt: "Transfer 250 dollars from checking to savings".to_string(),
        follow_up_prompts: vec!["What is my remaining checking balance?".to_string()],
        adversary: AdversaryConfig {
            hesitation_rate: 0.3,
            self_correction_rate: 0.2,
            attempt_barge_in: false,
            barge_in_delay_ms: 450,
        },
        reward_weights: GymRewardWeights::default(),
    };

    let scenario = TestScenario {
        id: "gym-banking-transfer".to_string(),
        name: "Gym Banking Transfer".to_string(),
        input: ScenarioInput::GymTask { task: gym_task },
        expected: None,
    };

    let serialized = serde_json::to_string(&scenario).expect("serialize scenario");
    let deserialized: TestScenario =
        serde_json::from_str(&serialized).expect("deserialize scenario");

    assert_eq!(deserialized.id, "gym-banking-transfer");
    if let ScenarioInput::GymTask { task } = deserialized.input {
        assert_eq!(task.domain, "banking");
        assert_eq!(
            task.initial_prompt,
            "Transfer 250 dollars from checking to savings"
        );
        assert_eq!(task.follow_up_prompts.len(), 1);
    } else {
        panic!("expected GymTask input");
    }
}

#[test]
fn test_gym_task_rollout_reward_computation() {
    let mut env = BankingTaskEnv::new_default();

    // Caller says: "Transfer 250 dollars from checking to savings"
    let call = ClientToolCall::new(
        "call-1",
        "transfer_funds",
        serde_json::json!({
            "from_account": "checking",
            "to_account": "savings",
            "amount": 250.0
        }),
    );

    let res = env.execute(&call).expect("transfer funds tool succeeded");
    assert_eq!(res["status"], "success");

    let goal = BankingGoal::Transfer {
        from_account: "checking".to_string(),
        to_account: "savings".to_string(),
        expected_amount: 250.0,
    };

    let eval = env.verify_goal(&goal);
    assert!(eval.success);
    assert_eq!(eval.score, 1.0);

    let weights = GymRewardWeights::default();
    let reward = GymRewardBreakdown::compute(
        &weights,
        &eval,
        1.0,         // perfect slot extraction
        0.5,         // smooth turn cadence
        Some(420.0), // 420ms TTFA (under 500ms budget)
        None,
    );

    assert_eq!(reward.task_reward, 1.0);
    assert_eq!(reward.slot_reward, 0.5);
    assert_eq!(reward.latency_penalty, 0.0);
    assert!((reward.total_reward - 1.6).abs() < 1e-4);

    let gym_result = GymResult {
        domain: "banking".to_string(),
        evaluation: eval.clone(),
        reward,
        tool_calls_executed: 1,
    };

    let scenario_result = ScenarioResult {
        scenario_id: "gym-banking-1".to_string(),
        scenario_name: "Gym Banking 1".to_string(),
        session_ids: vec!["session-42".to_string()],
        status: ScenarioRunStatus::Passed,
        metrics: ScenarioMetrics {
            e2e_latency_ms: Some(420.0),
            ..Default::default()
        },
        latency_samples: vec![420.0],
        scoring: None,
        error_message: None,
        repetitions_failed: None,
        tts_entries: None,
        turns: None,
        barge_in: None,
        synthetic_caller: None,
        gym: Some(gym_result),
    };

    assert_eq!(scenario_result.status, ScenarioRunStatus::Passed);
    assert!(scenario_result.gym.is_some());
}

#[test]
fn test_high_throughput_50_rollouts_in_memory() {
    let mut env = BankingTaskEnv::new_default();
    let adv = AcousticAdversary::default();
    let weights = GymRewardWeights::default();

    let start = Instant::now();

    for seed in 0..50 {
        env.reset();

        // 1. Generate adversarial prompt
        let prompt = adv.perturb_prompt("Transfer 100 from checking to savings", seed);
        assert!(!prompt.is_empty());

        // 2. Execute simulated agent tool action
        let call = ClientToolCall::new(
            "call-tx",
            "transfer_funds",
            serde_json::json!({
                "from_account": "checking",
                "to_account": "savings",
                "amount": 100.0
            }),
        );
        let _ = env.execute(&call).expect("execute");

        // 3. Verify outcome
        let goal = BankingGoal::Transfer {
            from_account: "checking".to_string(),
            to_account: "savings".to_string(),
            expected_amount: 100.0,
        };
        let eval = env.verify_goal(&goal);
        assert!(eval.success);

        // 4. Compute reward
        let reward = GymRewardBreakdown::compute(&weights, &eval, 1.0, 0.0, Some(350.0), None);
        assert!(reward.total_reward > 1.0);
    }

    let elapsed = start.elapsed();
    // 50 full rollouts and state verifications must complete in < 20 milliseconds
    assert!(
        elapsed.as_millis() < 20,
        "50 rollouts took {elapsed:?} (expected < 20ms)"
    );
}

#[test]
fn test_orders_support_gym_task_flow() {
    let mut env = OrderSupportTaskEnv::new_default();

    // 1. Check order status
    let check_call = ClientToolCall::new(
        "call-lookup",
        "lookup_order",
        serde_json::json!({ "order_id": "B482" }),
    );
    let check_res = env.execute(&check_call).expect("lookup order");
    assert_eq!(check_res["status"], "processing");

    // 2. Change address
    let update_call = ClientToolCall::new(
        "call-update",
        "update_shipping_address",
        serde_json::json!({
            "order_id": "B482",
            "new_address": "789 Pine Road, Springfield"
        }),
    );
    let update_res = env.execute(&update_call).expect("update address");
    assert_eq!(update_res["status"], "success");

    // 3. Verify goal
    let goal = OrderGoal::AddressUpdated {
        order_id: "B482".to_string(),
        expected_address: "789 Pine Road, Springfield".to_string(),
    };
    let eval = env.verify_goal(&goal);
    assert!(eval.success);
    assert_eq!(eval.score, 1.0);
}

#[test]
fn test_clinic_booking_gym_task_flow() {
    let mut env = ClinicBookingTaskEnv::new_default();

    // 1. Find available slot
    let slots_call = ClientToolCall::new(
        "call-slots",
        "find_slots",
        serde_json::json!({
            "doctor": "Dr. Smith",
            "date": "2026-11-04"
        }),
    );
    let slots_res = env.execute(&slots_call).expect("find slots");
    assert!(slots_res["available_times"].is_array());

    // 2. Book appointment
    let book_call = ClientToolCall::new(
        "call-book",
        "book_appointment",
        serde_json::json!({
            "patient_name": "Jordan Lee",
            "doctor": "Dr. Smith",
            "date": "2026-11-04",
            "time": "10:00"
        }),
    );
    let book_res = env.execute(&book_call).expect("book appointment");
    assert_eq!(book_res["status"], "confirmed");

    // 3. Verify goal
    let goal = ClinicGoal::AppointmentBooked {
        patient_name: "Jordan Lee".to_string(),
        doctor: "Dr. Smith".to_string(),
        date: "2026-11-04".to_string(),
        time: "10:00".to_string(),
    };
    let eval = env.verify_goal(&goal);
    assert!(eval.success);
    assert_eq!(eval.score, 1.0);
}

#[test]
fn test_gym_suite_json_example_parses_validly() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/benchmark/gym-suite.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let suite: TestSuite = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

    assert_eq!(suite.id, "suite-gym-native");
    assert_eq!(suite.scenarios.len(), 3);

    for s in &suite.scenarios {
        assert!(matches!(s.input, ScenarioInput::GymTask { .. }));
    }
}
