/**
 * Starter templates for the New Test Suite screen.
 *
 * Each template is a fully-formed TestSuite JSON document keyed by a short id.
 * They cover one example of every `ScenarioInput.type` the backend supports
 * (`text_prompt`, `audio_file`, `tts_only_corpus`, `conversation`, `barge_in`,
 * `synthetic_caller`) so a user can pick the closest match and tweak from there.
 *
 * `created_at` is filled in at selection time so each generated suite carries a
 * fresh ISO-8601 timestamp.
 */

export interface SuiteTemplate {
  id: string;
  label: string;
  description: string;
  /** Builds the JSON string shown in the editor textarea. */
  build: () => string;
}

const now = () => new Date().toISOString();

const EMPTY: SuiteTemplate = {
  id: 'empty',
  label: 'Empty (minimal scaffold)',
  description: 'Bare-bones text_prompt suite with one scenario — start from a blank slate.',
  build: () => JSON.stringify({
    id: 'my-suite-id',
    name: 'My Suite',
    description: 'Optional',
    created_at: now(),
    scenarios: [
      {
        id: 'scenario-1',
        name: 'Hello world',
        input: { type: 'text_prompt', text: 'Hello?' },
        expected: { transcript_contains: ['hello'] },
      },
    ],
  }, null, 2),
};

const TEXT_PROMPT: SuiteTemplate = {
  id: 'text-prompt',
  label: 'Text prompts (LLM smoke tests)',
  description: 'Bypasses STT — feeds raw text to the LLM and checks the response text.',
  build: () => JSON.stringify({
    id: 'suite-text-prompts',
    name: 'Text prompt smoke tests',
    description: 'Validates LLM + TTS path with hard-coded prompts; STT is bypassed.',
    created_at: now(),
    scenarios: [
      {
        id: 'greeting',
        name: 'simple greeting',
        input: { type: 'text_prompt', text: 'Hi there!' },
        expected: { transcript_contains: ['hello', 'hi'], max_latency_ms: 4000 },
      },
      {
        id: 'time-query',
        name: 'asks for the time',
        input: { type: 'text_prompt', text: 'What time is it?' },
        expected: { response_intent: 'time_query', max_latency_ms: 4000 },
      },
    ],
  }, null, 2),
};

const AUDIO_FILE: SuiteTemplate = {
  id: 'audio-file',
  label: 'Audio file (end-to-end STT → LLM → TTS)',
  description: 'Replays pre-recorded WAV files through the full pipeline and scores transcription quality.',
  build: () => JSON.stringify({
    id: 'suite-audio-probes',
    name: 'Audio file probes',
    description: 'Drives the full pipeline with captured WAVs. Paths are relative to the worker process CWD.',
    created_at: now(),
    scenarios: [
      {
        id: 'hello-wav',
        name: 'hello.wav',
        input: { type: 'audio_file', path: 'captures/hello.wav' },
        expected: {
          golden_transcript: 'hello',
          transcript_contains: ['hello'],
          max_latency_ms: 6000,
        },
      },
      {
        id: 'weather-wav',
        name: 'whats-the-weather.wav',
        input: { type: 'audio_file', path: 'captures/whats-the-weather.wav' },
        expected: {
          transcript_contains: ['weather'],
          max_latency_ms: 6000,
        },
      },
    ],
  }, null, 2),
};

const TTS_CORPUS: SuiteTemplate = {
  id: 'tts-corpus',
  label: 'TTS-only corpus (synthesis benchmark)',
  description: 'Runs the TTS engine over a list of utterances and reports per-entry TTFB / RTF.',
  build: () => JSON.stringify({
    id: 'suite-tts-corpus',
    name: 'TTS corpus benchmark',
    description: 'Synthesizes a small mixed corpus through TTS only. Bypasses VAD / STT / LLM.',
    created_at: now(),
    scenarios: [
      {
        id: 'mixed-corpus',
        name: 'mixed-length English utterances',
        input: {
          type: 'tts_only_corpus',
          entries: [
            { id: 'short-1', text: 'Hello.', language: 'en', bucket: 'short', class: 'plain' },
            { id: 'medium-1', text: 'The quick brown fox jumps over the lazy dog.', language: 'en', bucket: 'medium', class: 'plain' },
            { id: 'numerics-1', text: 'Call me at 555-1234 at 9:30 AM.', language: 'en', bucket: 'medium', class: 'numerics' },
            { id: 'long-1', text: 'In a small village by the river there lived an old clockmaker who could fix anything except the passage of time.', language: 'en', bucket: 'long', class: 'plain' },
          ],
        },
        expected: { max_latency_ms: 8000 },
      },
    ],
  }, null, 2),
};

const CONVERSATION: SuiteTemplate = {
  id: 'conversation',
  label: 'Multi-turn conversation (scripted)',
  description: 'Replays a fixed sequence of audio/text turns to test multi-turn coherence and per-turn latency.',
  build: () => JSON.stringify({
    id: 'suite-multi-turn',
    name: 'Multi-turn scripted conversation',
    description: 'Drives the pipeline with a fixed turn sequence and tracks per-turn latency drift.',
    created_at: now(),
    scenarios: [
      {
        id: 'support-handoff',
        name: 'support handoff (3 turns)',
        input: {
          type: 'conversation',
          turns: [
            {
              id: 't1',
              input: { type: 'text', text: 'Hi, I need help with my order.' },
              expected: { transcript_contains: ['order'] },
            },
            {
              id: 't2',
              input: { type: 'text', text: 'Order number is A-1234.' },
              expected: { transcript_contains: ['1234'] },
            },
            {
              id: 't3',
              input: { type: 'text', text: 'When will it arrive?' },
              expected: { transcript_contains: ['arrive', 'ship', 'deliver'] },
            },
          ],
        },
        expected: {
          turns_completed: 3,
          max_latency_ms_per_turn: 5000,
          max_latency_drift_pct: 25,
        },
      },
    ],
  }, null, 2),
};

const BARGE_IN: SuiteTemplate = {
  id: 'barge-in',
  label: 'Barge-in / interruption',
  description: 'Sends an initial turn, fires an interruption mid-TTS, then verifies recovery.',
  build: () => JSON.stringify({
    id: 'suite-barge-in',
    name: 'Barge-in correctness',
    description: 'Measures barge-in RTT and post-interruption recovery. Requires VAD or client cancel support.',
    created_at: now(),
    scenarios: [
      {
        id: 'client-cancel-mid-tts',
        name: 'client cancels 300 ms into TTS',
        input: {
          type: 'barge_in',
          initial: {
            id: 'long-answer',
            input: { type: 'text', text: 'Tell me a long story about a clockmaker.' },
          },
          interrupt: {
            mode: { kind: 'client_cancel' },
            fire_on: { kind: 'playback_offset', offset_ms: 300 },
          },
          follow_up: {
            id: 'follow-up',
            input: { type: 'text', text: 'Never mind, what time is it?' },
            expected: { transcript_contains: ['time'] },
          },
        },
        expected: {
          must_interrupt: true,
          interruption_trigger: 'client_cancel',
          max_barge_in_rtt_ms: 400,
          max_recovery_ms: 1500,
          follow_up_must_complete: true,
        },
      },
    ],
  }, null, 2),
};

const SYNTHETIC_CALLER_OPENAI: SuiteTemplate = {
  id: 'synthetic-caller-openai',
  label: 'Synthetic caller (OpenAI Realtime)',
  description: 'Drives multi-turn conversations using OpenAI Realtime as the synthetic user. Needs the synthetic-caller feature and OPENAI_API_KEY.',
  build: () => JSON.stringify({
    id: 'suite-synthetic-caller-openai',
    name: 'Synthetic caller (OpenAI Realtime)',
    description: 'Drives multi-turn conversations against the pipeline using the OpenAI Realtime API as the synthetic user. Requires the `synthetic-caller` cargo feature and OPENAI_API_KEY. Two scenarios: a cheap text-bridged probe (bypasses pipeline STT) and a full audio-bridged probe (exercises VAD/STT/LLM/TTS; per-utterance WAVs auto-captured under <captures_dir>/<session_id>/ when [pipeline.audit] enabled=true, served by the admin UI at /api/v1/sessions/<id>/audio/{in,out}). Cost guardrails enforced per-scenario.',
    created_at: now(),
    scenarios: [
      {
        id: 'synthetic-caller-openai-time-text',
        name: 'polite caller asks the time (text bridge)',
        input: {
          type: 'synthetic_caller',
          config: {
            caller: { kind: 'openai_realtime', model: 'gpt-realtime', voice: 'marin' },
            persona: '',
            stop: { max_turns: 3, max_duration_s: 30, max_caller_silence_s: 8 },
            persona_template: 'polite_time',
            bridge: 'text',
          },
        },
        expected: { min_completed_turns: 1, max_pipeline_turn_latency_ms: 8000 },
      },
      {
        id: 'synthetic-caller-openai-refund-audio',
        name: 'impatient refund caller (audio bridge)',
        input: {
          type: 'synthetic_caller',
          config: {
            caller: { kind: 'openai_realtime', model: 'gpt-realtime', voice: 'marin' },
            persona: '',
            stop: { max_turns: 4, max_duration_s: 45, max_caller_silence_s: 10 },
            persona_template: 'impatient_refund',
            bridge: 'audio',
          },
        },
        expected: { min_completed_turns: 1, max_pipeline_turn_latency_ms: 10000 },
      },
    ],
  }, null, 2),
};

const SYNTHETIC_CALLER_GEMINI: SuiteTemplate = {
  id: 'synthetic-caller-gemini',
  label: 'Synthetic caller (Gemini Live)',
  description: 'Same shape as the OpenAI template but routed through Gemini Live. Needs the synthetic-caller feature and GOOGLE_API_KEY.',
  build: () => JSON.stringify({
    id: 'suite-synthetic-caller-gemini',
    name: 'Synthetic caller (Gemini Live)',
    description: 'Drives multi-turn conversations against the pipeline using Gemini Live as the synthetic user. Requires the `synthetic-caller` cargo feature and GOOGLE_API_KEY.',
    created_at: now(),
    scenarios: [
      {
        id: 'synthetic-caller-gemini-time-text',
        name: 'polite caller asks the time (text bridge)',
        input: {
          type: 'synthetic_caller',
          config: {
            caller: { kind: 'gemini_live', model: 'gemini-2.0-flash-exp', voice: 'Puck' },
            persona: '',
            stop: { max_turns: 3, max_duration_s: 30, max_caller_silence_s: 8 },
            persona_template: 'polite_time',
            bridge: 'text',
          },
        },
        expected: { min_completed_turns: 1, max_pipeline_turn_latency_ms: 8000 },
      },
    ],
  }, null, 2),
};

const GYM_NATIVE: SuiteTemplate = {
  id: 'gym-native',
  label: 'Voice Pipes Gym (Native RL Tasks)',
  description: 'In-memory task environments (Banking, Orders, Clinic) testing tool calls, acoustic hesitations, self-corrections, and multi-modal rewards.',
  build: () => JSON.stringify({
    id: 'suite-gym-native',
    name: 'Voice Pipes Gym Native Evaluation Suite',
    description: 'Native in-memory RL task environments testing voice agent tool-calling, slot accuracy, and resilience to acoustic perturbations without external services.',
    created_at: now(),
    scenarios: [
      {
        id: 'gym-banking-transfer',
        name: 'Banking: Funds Transfer with Spoken Hesitation',
        input: {
          type: 'gym_task',
          config: {
            domain: 'banking',
            goal: {
              type: 'transfer',
              from_account: 'checking',
              to_account: 'savings',
              expected_amount: 250.0,
            },
            initial_prompt: 'Transfer 250 dollars from checking to savings',
            follow_up_prompts: ['What is my remaining checking balance?'],
            adversary: {
              hesitation_rate: 0.3,
              self_correction_rate: 0.2,
              attempt_barge_in: false,
              barge_in_delay_ms: 450,
            },
            reward_weights: {
              task_success_weight: 1.0,
              slot_accuracy_weight: 0.5,
              turn_etiquette_weight: 0.2,
              latency_target_ms: 500.0,
              latency_penalty_factor: 0.002,
              barge_in_overrun_penalty: 0.005,
            },
          },
        },
      },
      {
        id: 'gym-order-address-update',
        name: 'Order Support: Address Change with Alphanumeric ID',
        input: {
          type: 'gym_task',
          config: {
            domain: 'order_support',
            goal: {
              type: 'address_updated',
              order_id: 'B482',
              expected_address: '789 Pine Road, Springfield',
            },
            initial_prompt: 'I need to update the delivery address on order B482 to 789 Pine Road, Springfield',
            follow_up_prompts: [],
            adversary: {
              hesitation_rate: 0.2,
              self_correction_rate: 0.1,
              attempt_barge_in: false,
              barge_in_delay_ms: 500,
            },
            reward_weights: {
              task_success_weight: 1.0,
              slot_accuracy_weight: 0.5,
              turn_etiquette_weight: 0.2,
              latency_target_ms: 500.0,
              latency_penalty_factor: 0.002,
              barge_in_overrun_penalty: 0.005,
            },
          },
        },
      },
      {
        id: 'gym-clinic-booking',
        name: 'Clinic: Doctor Appointment Booking',
        input: {
          type: 'gym_task',
          config: {
            domain: 'clinic_booking',
            goal: {
              type: 'appointment_booked',
              patient_name: 'Jordan Lee',
              doctor: 'Dr. Smith',
              date: '2026-11-04',
              time: '10:00',
            },
            initial_prompt: 'Can you book an appointment with Dr. Smith on November 4th at 10 AM for Jordan Lee?',
            follow_up_prompts: [],
            adversary: {
              hesitation_rate: 0.1,
              self_correction_rate: 0.0,
              attempt_barge_in: false,
              barge_in_delay_ms: 500,
            },
            reward_weights: {
              task_success_weight: 1.0,
              slot_accuracy_weight: 0.5,
              turn_etiquette_weight: 0.2,
              latency_target_ms: 500.0,
              latency_penalty_factor: 0.002,
              barge_in_overrun_penalty: 0.005,
            },
          },
        },
      },
    ],
  }, null, 2),
};

export const SUITE_TEMPLATES: SuiteTemplate[] = [
  EMPTY,
  GYM_NATIVE,
  TEXT_PROMPT,
  AUDIO_FILE,
  TTS_CORPUS,
  CONVERSATION,
  BARGE_IN,
  SYNTHETIC_CALLER_OPENAI,
  SYNTHETIC_CALLER_GEMINI,
];
