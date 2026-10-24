# Voice Pipes Gym — Architecture & Specification

**Status:** active
**Last verified:** 2026-10-24
**Owner:** Docs

## 1. Overview

**Voice Pipes Gym** is an interactive, native Reinforcement Learning (RL) environment and automated regression testing suite for real-time voice agents. 

Unlike traditional LLM agent gyms (such as Eigent AI's `toolathlon_gym`, `AgentGym`, or `SWE-gym`) that evaluate agents on static, discrete text turns, Voice Pipes Gym evaluates and trains agents under real-world, full-duplex conversational voice conditions.

The system is designed **standalone and zero-dependency first**:
- **Pure In-Memory State**: Native in-tree task state machines requiring zero external databases, Docker containers, or Python dependencies.
- **Microsecond Resets**: Fast snapshot rollback between episodes.
- **Voice-Native Perturbations**: Simulates caller hesitations, mid-sentence self-corrections, background noise, and barge-in interruptions.
- **Cascaded & S2S Support**: Native support for open-source cascaded pipelines (STT + LLM + TTS) as well as end-to-end Speech-to-Speech (S2S) models.
- **Self-Hostable on 1–2 Consumer/Pro GPUs**: Trainable locally using GRPO/DPO with open-source models (Parakeet, Qwen 2.5 7B, Piper).
- **Optional MCP / Toolathlon Bridge**: Built on standard `ClientToolSpec` schemas so external MCP servers (like Toolathlon) can be attached as optional adapters without changing the core engine.

---

## 2. Core Workspace Architecture

```
┌────────────────────────────────────────────────────────────────────────┐
│                   Layer 3: Python RL Bridge (Optional)                 │
│      voice-pipes-gym-ffi (PyO3) -> Gymnasium Env API for TRL / Ray    │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
┌───────────────────────────────────▼────────────────────────────────────┐
│         Layer 2: Voice Pipes Bench & Virtual-Clock Rollout             │
│   • Accelerated Virtual Clock (inject_speed = 0.0 AFAP, 10x-50x speed)  │
│   • Acoustic & Disfluency Adversary (hesitation, accents, barge-in)    │
│   • Composite Multi-Modal Reward Evaluator (Task + Perception + Timing)│
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
┌───────────────────────────────────▼────────────────────────────────────┐
│                   Layer 1: Native Task Environment Seam                │
│                     trait TaskEnvironment                             │
│       ├── Native In-Memory Tasks (Banking, Orders, Clinic) [Default]  │
│       └── McpBridge (Optional adapter for external MCP / Toolathlon)   │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │
┌───────────────────────────────────▼────────────────────────────────────┐
│                    Layer 0: Voice Pipes Core Runtime                   │
│         16kHz PCM audio routing, VAD, Smart Turn, VAP, Single Clock     │
└────────────────────────────────────────────────────────────────────────┘
```

### New / Extended Crates:
1. **`voice-pipes-gym`**:
   - Defines the `TaskEnvironment` trait and houses the 3 native, in-memory voice task environments.
   - Self-contained, `#![forbid(unsafe_code)]`, zero external network calls.
2. **`voice-pipes-bench`**:
   - Extended with the virtual-clock simulation driver (`inject_speed = 0.0` AFAP).
   - Acoustic adversary for synthetic callers (speech hesitations, self-corrections, background noise).
   - Composite reward evaluator.
3. **`voice-pipes-gym-ffi` (Optional FFI crate)**:
   - PyO3 wrapper exposing Gymnasium-compliant bindings (`voicepipes_gym`) for Python-based RL trainers (TRL, NeMo Gym, Ray/RLlib).
4. **`voice-pipes-mcp-bridge` (Future / Optional)**:
   - Adapter that implements `TaskEnvironment` over external stdio / HTTP MCP servers (e.g. Toolathlon).

---

## 3. Native Task Environments (Zero External Dependencies)

All tasks implement the standard `voice-pipes-sdk::agent::ClientToolSpec` and `ClientToolCall` interfaces:

### Domain A: Banking & Transfers (`VoiceGym-Banking`)
- **Tools**:
  - `get_balance(account_id)`
  - `transfer_funds(from_account, to_account, amount)`
  - `lock_card(card_last4)`
- **Acoustic Challenge**: Mid-utterance corrections (*"Send \$150 to account 302... wait, no, cancel that, make it account 305"*).
- **Verification**: In-memory balance balances match exact expectation with no orphaned transactions.

### Domain B: Logistics & Order Support (`VoiceGym-OrderSupport`)
- **Tools**:
  - `lookup_order(order_id)`
  - `update_shipping_address(order_id, new_address)`
  - `cancel_order(order_id, reason)`
- **Acoustic Challenge**: Alphanumeric spelling with background noise (*"Order ID is B-as-in-Boy, 7, 9, K"*).
- **Verification**: Target order reflects modified address with correct casing and zero corruption.

### Domain C: Clinic & Appointments (`VoiceGym-ClinicBooking`)
- **Tools**:
  - `find_slots(doctor_id, date_range)`
  - `book_appointment(doctor_id, date, time, patient_name)`
  - `reschedule_appointment(appointment_id, new_date, new_time)`
- **Acoustic Challenge**: Multi-turn slot negotiation where initial requested slot is unavailable.
- **Verification**: Exactly one confirmed slot reserved matching patient preference.

---

## 4. Multi-Modal Composite Reward Function

$$\mathcal{R} = R_{\text{task}} + R_{\text{args}} + R_{\text{turn}} - P_{\text{latency}} - P_{\text{barge\_in}}$$

1. **$R_{\text{task}} \in \{0.0, 1.0\}$ (Outcome Reward)**:
   Verifiable state mutation in the task environment (e.g., account balance delta matches expected transfer).
2. **$R_{\text{args}} \in [0.0, 1.0]$ (Perception Reward)**:
   Accuracy of extracted slot values despite acoustic noise or accents.
3. **$R_{\text{turn}} \in [-0.5, 0.5]$ (Conversational Etiquette)**:
   Turn-taking score rewarding natural pauses and penalizing speaking over user speech.
4. **$P_{\text{latency}}$ (Speed Penalty)**:
   $$\max\left(0, \frac{\text{TTFA} - 500\text{ms}}{1000\text{ms}}\right)$$
   Penalizes Time-to-First-Audio exceeding the 500ms real-time conversational budget.
5. **$P_{\text{barge\_in}}$ (Interruption Overrun Penalty)**:
   Penalizes agent audio generation that fails to stop within $200\text{ms}$ of user interruption.

---

## 5. Faster-Than-Real-Time (AFAP) Audio Simulation

While live phone calls are pinned to real-time wall-clock physics ($1.0\times$ Real-Time Factor / RTF), computers have no vocal cords. Voice Pipes operates in **As-Fast-As-Possible (AFAP)** mode (`inject_speed = 0.0`):

- **Silero VAD**: Evaluates audio chunks at $\text{RTF} = 0.005$ ($25\text{ms}$ for $5\text{s}$ of speech).
- **Parakeet STT**: Streaming acoustic encoder transcribes at $\text{RTF} = 0.020$ ($100\text{ms}$ for $5\text{s}$).
- **Piper TTS**: Synthesizes speech at $\text{RTF} = 0.015$ ($75\text{ms}$ for $5\text{s}$).
- **Gym State Machine**: Transactional in-memory state transition executes in $<30\mu\text{s}$.

A full 10-second spoken phone conversation executes glass-to-glass in **$\sim 275\text{ms}$ total** ($35\times$ to $40\times$ faster than real time). This enables:
- **10,000 simulated phone calls in ~40 minutes** on a single workstation GPU.
- High-throughput parallel rollouts ($K=4\text{ to }8$) per prompt for **GRPO / DPO** reinforcement learning.
- Training models directly on noisy phonetic transcripts with hesitation pauses and speech repairs, eliminating the gap between text-only training and acoustic reality.

---

## 6. Local Hardware Footprint & Cascaded Open-Source Deployment

Voice Pipes Gym does not require enterprise clusters or proprietary APIs. It runs efficiently on **1 or 2 workstation/consumer GPUs** (e.g. RTX 4090 24GB or A100 40/80GB):

| Component | Recommended Model | Execution Target | VRAM Footprint |
| :--- | :--- | :--- | :--- |
| **STT** | Parakeet TDT 0.6B / Moonshine | CPU / CUDA (ONNX) | ~1.2 GB (or 0 GB on CPU) |
| **TTS** | Piper VITS / Kokoro | CPU / CUDA (ONNX) | ~0.4 GB (or 0 GB on CPU) |
| **VAD / Turn** | Silero VAD + Smart Turn | CPU (ONNX) | ~0.1 GB (0 GB on GPU) |
| **Policy LLM** | Qwen 2.5 7B / Llama 3.1 8B (4-bit) | GPU (vLLM / llama-server) | ~6.5 GB |
| **KV Cache & Rollouts** | 8 concurrent virtual rollouts | GPU | ~4.0 GB |
| **Total** | | | **~12.2 GB / 24 GB VRAM** |

> **CPU Offloading Advantage:** Because STT, TTS, and VAD run comfortably on multi-core CPUs via ONNX Runtime without latency degradation, almost the entire GPU VRAM is dedicated to the policy LLM and active RL training gradients.

---

## 7. How We Use It Ourselves

### Use Case A: Train a Voice-Aligned Open LLM via GRPO / LoRA
- Standard text LLMs write long paragraphs and fail on speech disfluencies.
- Using **Group Relative Policy Optimization (GRPO)** with LoRA on an open 7B/8B model, the Gym runs 4–8 virtual-time rollouts per prompt.
- Rewards incentivize concise spoken dialogue, correct tool execution despite noisy phonetics, and instant mid-sentence correction handling.

### Use Case B: Turn-Taking & VAD Calibration
- Calibrate `voice-pipes-turn-smart` and `voice-pipes-turn-vap` (Voice Activity Projection) thresholds against adversarial hesitations (300–600ms pauses) in under 30 minutes.

### Use Case C: Automated Voice CI/CD Regression Testing
- Prior to deploying any prompt, tool, or audio model update:
  ```sh
  cargo test -p voice-pipes-bench --test gym_eval
  ```
- Evaluates 100 simulated calls in under 60 seconds with synthetic acoustic perturbations, asserting that pass rates and latency bounds ($<500\text{ms}$) do not regress.

---

## 8. Architectural Invariants

In accordance with Voice Pipes standards:
- `#![forbid(unsafe_code)]` in all new crates.
- Compiles with `-D warnings` and zero warnings.
- Non-test code strictly denies indexing slicing and panicking unwraps.
- Missing metrics are `null`, never `0`. Trait-seam single clock is used for latency measurement.
- Reset time per episode must remain $< 100\mu\text{s}$ in-memory.
