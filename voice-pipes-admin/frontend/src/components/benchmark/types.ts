// ── Benchmark domain types (mirrors Rust types.rs) ──────

export type ExperimentStatus = 'draft' | 'running' | 'complete' | 'failed';
export type ScenarioRunStatus = 'passed' | 'failed' | 'timeout' | 'error';

export interface Variant {
  id: string;
  name: string;
  config_path: string;
  config_hash: string;
  tags: string[];
}

export interface Experiment {
  id: string;
  name: string;
  description?: string;
  created_at: string;
  started_at?: string;
  completed_at?: string;
  status: ExperimentStatus;
  test_suite_id: string;
  variants: Variant[];
  repetitions: number;
}

export interface TtsCorpusEntry {
  id: string;
  text: string;
  language?: string;
  bucket?: 'short' | 'medium' | 'long';
  class?: 'plain' | 'numerics' | 'punctuation' | 'mixed';
}

export type ScenarioInputType =
  | 'audio_file'
  | 'text_prompt'
  | 'tts_only_corpus'
  | 'conversation'
  | 'barge_in'
  | 'synthetic_caller'
  | 'gym_task';

export interface GymTaskConfig {
  domain: string;
  goal: Record<string, unknown>;
  initial_prompt: string;
  follow_up_prompts?: string[];
  adversary?: {
    hesitation_rate?: number;
    self_correction_rate?: number;
    attempt_barge_in?: boolean;
    barge_in_delay_ms?: number;
  };
  reward_weights?: {
    task_success_weight?: number;
    slot_accuracy_weight?: number;
    turn_etiquette_weight?: number;
    latency_target_ms?: number;
    latency_penalty_factor?: number;
    barge_in_overrun_penalty?: number;
  };
}

export interface ScenarioInput {
  type: ScenarioInputType;
  // audio_file / text_prompt / tts_only_corpus
  path?: string;
  text?: string;
  entries?: TtsCorpusEntry[];
  // conversation
  turns?: ConversationTurn[];
  // barge_in
  initial?: ConversationTurn;
  interrupt?: InterruptSpec;
  follow_up?: ConversationTurn | null;
  // synthetic_caller (serde renames `caller` → `config` on the wire)
  config?: SyntheticCallerConfig;
}

export interface TurnInput {
  type: 'audio' | 'text';
  path?: string;
  text?: string;
}

export interface TurnExpected {
  transcript_contains?: string[];
  golden_transcript?: string;
  max_latency_ms?: number;
}

export interface ConversationTurn {
  id: string;
  input: TurnInput;
  expected?: TurnExpected;
}

export type InterruptModeKind = 'client_cancel' | 'inject_audio' | 'inject_text';

export interface InterruptMode {
  kind: InterruptModeKind;
  path?: string;
  text?: string;
}

export type InterruptFireOnKind = 'tts_started' | 'first_audio' | 'playback_offset';

export interface InterruptFireOn {
  kind: InterruptFireOnKind;
  offset_ms?: number;
}

export interface InterruptSpec {
  mode: InterruptMode;
  fire_on: InterruptFireOn;
}

export type CallerKindTag = 'mock' | 'openai_realtime' | 'gemini_live';

export interface CallerKind {
  kind: CallerKindTag;
  script?: string;
  model?: string;
  voice?: string;
}

export interface SyntheticStopConditions {
  max_turns: number;
  max_duration_s: number;
  max_caller_silence_s?: number;
}

export interface BargeInPolicy {
  every_n_turns: number;
  max_consecutive_failures?: number;
}

export type BridgeMode = 'text' | 'audio';

export interface SyntheticCallerConfig {
  caller: CallerKind;
  persona: string;
  stop: SyntheticStopConditions;
  barge_in?: BargeInPolicy;
  persona_template?: string;
  bridge?: BridgeMode;
}

export type ExpectedTrigger = 'server_vad' | 'client_cancel' | 'early_barge' | 'any';

export interface ScenarioExpected {
  transcript_contains?: string[];
  response_intent?: string;
  max_latency_ms?: number;
  golden_transcript?: string;
  golden_response?: string;
  // Multi-turn
  turns_completed?: number;
  max_latency_ms_per_turn?: number;
  max_latency_drift_pct?: number;
  // Barge-in
  must_interrupt?: boolean;
  interruption_trigger?: ExpectedTrigger;
  max_barge_in_rtt_ms?: number;
  max_recovery_ms?: number;
  follow_up_must_complete?: boolean;
  // Synthetic-caller
  min_completed_turns?: number;
  pipeline_must_say?: string[];
  max_pipeline_turn_latency_ms?: number;
  max_barge_in_failure_rate?: number;
}

export interface TestScenario {
  id: string;
  name: string;
  input: ScenarioInput;
  expected?: ScenarioExpected;
}

export interface TestSuite {
  id: string;
  name: string;
  description?: string;
  created_at: string;
  scenarios: TestScenario[];
}

export interface ScenarioMetrics {
  e2e_latency_ms: number | null;
  ttft_ms: number | null;
  ttfb_ms: number | null;
  stt_text: string | null;
  response_text: string | null;
  audio_duration_s: number | null;
  stt_latency_ms?: number | null;
  llm_generation_ms?: number | null;
  llm_tokens?: number | null;
  tts_ttfb_ms?: number | null;
}

export interface ScenarioScoring {
  wer?: number;
  cer?: number;
  quality_score?: number;
  quality_reasoning?: string;
  expectations_met: boolean;
  expectation_failures: string[];
}

export interface TaskEvaluation {
  success: boolean;
  score: number;
  message: string;
}

export interface GymRewardBreakdown {
  task_reward: number;
  slot_reward: number;
  turn_reward: number;
  latency_penalty: number;
  barge_in_penalty: number;
  total_reward: number;
}

export interface GymResult {
  domain: string;
  evaluation: TaskEvaluation;
  reward: GymRewardBreakdown;
  tool_calls_executed: number;
}

export interface ScenarioResult {
  scenario_id: string;
  scenario_name: string;
  session_ids: string[];
  status: ScenarioRunStatus;
  metrics: ScenarioMetrics;
  latency_samples: number[];
  scoring?: ScenarioScoring;
  error_message?: string;
  repetitions_failed?: number;
  /** Per-entry results for `tts_only_corpus` scenarios; `null` otherwise. */
  tts_entries?: TtsEntryResult[] | null;
  /** Per-turn results for `conversation` scenarios. */
  turns?: TurnResult[] | null;
  /** Barge-in measurements for `barge_in` scenarios. */
  barge_in?: BargeInResult | null;
  /** Synthetic-caller transcript / stop reason for `synthetic_caller` scenarios. */
  synthetic_caller?: SyntheticCallerResult | null;
  /** Voice Pipes Gym task outcome, goal evaluation & composite RL reward. */
  gym?: GymResult | null;
}

export interface TurnResult {
  turn_id: string;
  utterance_id?: number | null;
  stt_text?: string | null;
  response_text?: string | null;
  e2e_latency_ms?: number | null;
  completed?: boolean;
  expectation_failures?: string[];
  error?: string | null;
}

export interface BargeInResult {
  interrupted?: boolean;
  trigger?: string | null;
  barge_in_rtt_ms?: number | null;
  recovery_ms?: number | null;
  follow_up_completed?: boolean;
}

export type SyntheticStopReason =
  | 'max_turns'
  | 'max_duration'
  | 'caller_silence'
  | 'caller_error'
  | 'pipeline_error'
  | 'barge_in_failure_cap';

export interface SyntheticTurn {
  speaker: string;
  turn_index: number;
  transcript?: string | null;
  latency_ms?: number | null;
  ttft_ms?: number | null;
  ttfb_ms?: number | null;
  llm_response_text?: string | null;
  barge_in?: boolean;
}

export interface SyntheticCallerResult {
  turns: SyntheticTurn[];
  stop_reason?: SyntheticStopReason | null;
  total_duration_ms?: number | null;
  barge_in_attempts: number;
  barge_in_successes: number;
}

export type TtsLengthBucket = 'short' | 'medium' | 'long';
export type TtsContentClass = 'plain' | 'numerics' | 'punctuation' | 'mixed';

export interface TtsEntryResult {
  entry_id: string;
  text: string;
  bucket: TtsLengthBucket;
  class: TtsContentClass;
  ttfb_ms?: number | null;
  synthesis_ms?: number | null;
  audio_duration_ms?: number | null;
  sample_rate?: number | null;
  chars?: number | null;
  rtf?: number | null;
  cold_start?: boolean;
  provider?: string | null;
  /** Worker-host filesystem path. Never used directly by the UI — the
   *  audio endpoint resolves it server-side. */
  audio_path?: string | null;
  audio_available?: boolean;
  error?: string | null;
  /** Peak absolute amplitude (int16 scale) reported by the worker. */
  peak_amplitude?: number | null;
  /** Fraction (0–1) of 100 ms windows with speech-level energy. */
  speech_ratio?: number | null;
  /** The provider returned audio with no speech in it. */
  silent?: boolean;
  transcript?: string | null;
  wer?: number | null;
  cer?: number | null;
}

export interface VariantSummary {
  scenarios_run: number;
  scenarios_passed: number;
  scenarios_failed: number;
  avg_e2e_latency_ms: number | null;
  p50_e2e_latency_ms: number | null;
  p90_e2e_latency_ms: number | null;
  p99_e2e_latency_ms: number | null;
  avg_ttft_ms: number | null;
  avg_ttfb_ms: number | null;
  avg_wer?: number | null;
  avg_quality_score?: number | null;
  total_cost_estimate?: number | null;
  avg_stt_latency_ms?: number | null;
  avg_llm_generation_ms?: number | null;
  avg_tts_ttfb_ms?: number | null;
  p50_tts_ttfb_ms?: number | null;
  p90_tts_ttfb_ms?: number | null;
  p99_tts_ttfb_ms?: number | null;
  avg_tts_synthesis_ms?: number | null;
  avg_tts_audio_duration_s?: number | null;
  avg_tts_rtf?: number | null;
  avg_tts_chars_per_second?: number | null;
  /** Fraction of repetitions where TTS produced no audio (0.0 – 1.0). */
  tts_failure_rate?: number | null;
  /** Mean synthesis time of cold-start reps only. */
  cold_tts_synthesis_ms?: number | null;
  /** Mean synthesis time of warm reps only. */
  warm_tts_synthesis_ms?: number | null;
  avg_tokens_per_second?: number | null;
  // ── Multi-turn / barge-in aggregates ──
  conversation_scenarios?: number | null;
  avg_turns_completed?: number | null;
  avg_turn_latency_ms?: number | null;
  barge_in_scenarios?: number | null;
  /** Fraction (0.0 – 1.0) of barge-in scenarios where the pipeline interrupted. */
  barge_in_success_rate?: number | null;
  p50_barge_in_rtt_ms?: number | null;
  p95_barge_in_rtt_ms?: number | null;
  p50_recovery_ms?: number | null;
  p95_recovery_ms?: number | null;
}

export interface VariantResults {
  variant_id: string;
  variant_name: string;
  scenarios: ScenarioResult[];
  summary: VariantSummary;
  config_summary?: PipelineConfigSummary;
  error_reason?: string;
}

export interface PipelineConfigSummary {
  pipeline_name?: string;
  vad_mode?: string;
  stt_provider?: string;
  stt_model?: string;
  llm_model?: string;
  llm_base_url?: string;
  llm_temperature?: number;
  llm_max_tokens?: number;
  llm_system_prompt?: string;
  tts_provider?: string;
  tts_model?: string;
  turn_detection_mode?: string;
  silence_timeout_ms?: number;
  barge_in_enabled?: boolean;
}

export interface ExperimentResults {
  experiment_id: string;
  completed_at: string;
  variant_results: VariantResults[];
}
