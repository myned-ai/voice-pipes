import { useState } from 'react';
import { ChevronRight, ChevronDown, Mic, FileText, BookText, MessagesSquare, Zap, Bot, Trophy } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { TestScenario, ConversationTurn, TurnExpected, InterruptSpec, SyntheticCallerConfig } from './types';

interface Props {
  scenario: TestScenario;
  defaultExpanded?: boolean;
}

export default function ScenarioRow({ scenario, defaultExpanded = false }: Props) {
  const { isDark } = useTheme();
  const [expanded, setExpanded] = useState(defaultExpanded);

  const inputIcon = (() => {
    switch (scenario.input.type) {
      case 'audio_file':       return <Mic size={14} className="text-indigo-400" />;
      case 'tts_only_corpus':  return <BookText size={14} className="text-purple-400" />;
      case 'conversation':     return <MessagesSquare size={14} className="text-cyan-400" />;
      case 'barge_in':         return <Zap size={14} className="text-rose-400" />;
      case 'synthetic_caller': return <Bot size={14} className="text-emerald-400" />;
      case 'gym_task':         return <Trophy size={14} className="text-amber-400" />;
      case 'text_prompt':
      default:                 return <FileText size={14} className="text-amber-400" />;
    }
  })();

  const inputLabel = (() => {
    switch (scenario.input.type) {
      case 'audio_file':       return 'audio';
      case 'tts_only_corpus':  return `corpus (${scenario.input.entries?.length ?? 0})`;
      case 'conversation':     return `conv (${scenario.input.turns?.length ?? 0} turns)`;
      case 'barge_in':         return `barge: ${scenario.input.interrupt?.mode.kind ?? '?'}`;
      case 'synthetic_caller': return `caller: ${(scenario.input.config as { caller?: { kind?: string } })?.caller?.kind ?? '?'}`;
      case 'gym_task':         return `gym: ${(scenario.input.config as { domain?: string })?.domain ?? 'task'}`;
      case 'text_prompt':
      default:                 return 'text';
    }
  })();

  // Build a brief expected summary for the collapsed view
  const expectedBrief = buildExpectedBrief(scenario);

  return (
    <div className={`border-b last:border-b-0 ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
      {/* Collapsed row */}
      <button
        onClick={() => setExpanded(!expanded)}
        className={`w-full flex items-center gap-3 px-4 py-3 text-left transition-colors ${
          isDark ? 'hover:bg-slate-700/50' : 'hover:bg-gray-50'
        }`}
      >
        <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </span>
        <span className={`text-sm font-medium flex-1 ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>
          {scenario.name}
        </span>
        <span className="flex items-center gap-1.5">
          {inputIcon}
          <span className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{inputLabel}</span>
        </span>
        {expectedBrief && (
          <span className={`text-xs truncate max-w-[200px] ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            {expectedBrief}
          </span>
        )}
      </button>

      {/* Expanded details */}
      {expanded && (
        <div className={`px-4 pb-4 pl-10 ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
          {/* Input details */}
          <div className="mb-3">
            <span className={`text-xs font-medium uppercase tracking-wide ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
              Input
            </span>
            <div className="mt-1 text-sm">
              {scenario.input.type === 'audio_file' && scenario.input.path && (
                <span className="font-mono text-xs">{scenario.input.path}</span>
              )}
              {scenario.input.type === 'text_prompt' && scenario.input.text && (
                <span className="italic">"{scenario.input.text}"</span>
              )}
              {scenario.input.type === 'tts_only_corpus' && scenario.input.entries && (
                <table className="w-full text-xs mt-1 border-collapse">
                  <thead>
                    <tr className={isDark ? 'text-slate-500' : 'text-gray-400'}>
                      <th className="text-left pr-3 pb-1 font-medium">ID</th>
                      <th className="text-left pr-3 pb-1 font-medium">Bucket</th>
                      <th className="text-left pr-3 pb-1 font-medium">Class</th>
                      <th className="text-left pb-1 font-medium">Text</th>
                    </tr>
                  </thead>
                  <tbody>
                    {scenario.input.entries.map(e => (
                      <tr key={e.id} className={isDark ? 'border-t border-slate-700' : 'border-t border-gray-100'}>
                        <td className={`pr-3 py-1 font-mono ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{e.id}</td>
                        <td className={`pr-3 py-1 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{e.bucket ?? '—'}</td>
                        <td className={`pr-3 py-1 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{e.class ?? '—'}</td>
                        <td className={isDark ? 'text-slate-300 py-1' : 'text-gray-700 py-1'}>{e.text}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
              {scenario.input.type === 'conversation' && scenario.input.turns && (
                <ConversationTurnList turns={scenario.input.turns} isDark={isDark} />
              )}
              {scenario.input.type === 'barge_in' && (
                <BargeInPreview
                  initial={scenario.input.initial}
                  interrupt={scenario.input.interrupt}
                  followUp={scenario.input.follow_up ?? undefined}
                  isDark={isDark}
                />
              )}
              {scenario.input.type === 'synthetic_caller' && scenario.input.config && (
                <SyntheticCallerPreview config={scenario.input.config} isDark={isDark} />
              )}
            </div>
          </div>

          {/* Expected outcomes */}
          {scenario.expected && (
            <div>
              <span className={`text-xs font-medium uppercase tracking-wide ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                Expected
              </span>
              <div className="mt-1 grid gap-1.5">
                {scenario.expected.transcript_contains && (
                  <ExpectedField
                    label="transcript_contains"
                    value={JSON.stringify(scenario.expected.transcript_contains)}
                    isDark={isDark}
                  />
                )}
                {scenario.expected.response_intent && (
                  <ExpectedField
                    label="response_intent"
                    value={scenario.expected.response_intent}
                    isDark={isDark}
                  />
                )}
                {scenario.expected.max_latency_ms != null && (
                  <ExpectedField
                    label="max_latency_ms"
                    value={`${scenario.expected.max_latency_ms} ms`}
                    isDark={isDark}
                  />
                )}
                {scenario.expected.golden_transcript && (
                  <ExpectedField
                    label="golden_transcript"
                    value={`"${scenario.expected.golden_transcript}"`}
                    isDark={isDark}
                  />
                )}
                {scenario.expected.golden_response && (
                  <ExpectedField
                    label="golden_response"
                    value={`"${scenario.expected.golden_response}"`}
                    isDark={isDark}
                  />
                )}
                {/* Multi-turn / barge-in / synthetic-caller assertions */}
                {scenario.expected.turns_completed != null && (
                  <ExpectedField label="turns_completed" value={String(scenario.expected.turns_completed)} isDark={isDark} />
                )}
                {scenario.expected.max_latency_ms_per_turn != null && (
                  <ExpectedField label="max_latency_ms_per_turn" value={`${scenario.expected.max_latency_ms_per_turn} ms`} isDark={isDark} />
                )}
                {scenario.expected.max_latency_drift_pct != null && (
                  <ExpectedField label="max_latency_drift_pct" value={`${scenario.expected.max_latency_drift_pct}%`} isDark={isDark} />
                )}
                {scenario.expected.must_interrupt && (
                  <ExpectedField label="must_interrupt" value="true" isDark={isDark} />
                )}
                {scenario.expected.interruption_trigger && (
                  <ExpectedField label="interruption_trigger" value={scenario.expected.interruption_trigger} isDark={isDark} />
                )}
                {scenario.expected.max_barge_in_rtt_ms != null && (
                  <ExpectedField label="max_barge_in_rtt_ms" value={`${scenario.expected.max_barge_in_rtt_ms} ms`} isDark={isDark} />
                )}
                {scenario.expected.max_recovery_ms != null && (
                  <ExpectedField label="max_recovery_ms" value={`${scenario.expected.max_recovery_ms} ms`} isDark={isDark} />
                )}
                {scenario.expected.follow_up_must_complete && (
                  <ExpectedField label="follow_up_must_complete" value="true" isDark={isDark} />
                )}
                {scenario.expected.min_completed_turns != null && (
                  <ExpectedField label="min_completed_turns" value={String(scenario.expected.min_completed_turns)} isDark={isDark} />
                )}
                {scenario.expected.pipeline_must_say && scenario.expected.pipeline_must_say.length > 0 && (
                  <ExpectedField label="pipeline_must_say" value={JSON.stringify(scenario.expected.pipeline_must_say)} isDark={isDark} />
                )}
                {scenario.expected.max_pipeline_turn_latency_ms != null && (
                  <ExpectedField label="max_pipeline_turn_latency_ms" value={`${scenario.expected.max_pipeline_turn_latency_ms} ms`} isDark={isDark} />
                )}
                {scenario.expected.max_barge_in_failure_rate != null && (
                  <ExpectedField label="max_barge_in_failure_rate" value={`${(scenario.expected.max_barge_in_failure_rate * 100).toFixed(0)}%`} isDark={isDark} />
                )}
              </div>
            </div>
          )}

          {!scenario.expected && (
            <span className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
              {hasTurnExpectations(scenario)
                ? 'Per-turn assertions only (shown inline on the turns above)'
                : 'No expected outcomes defined'}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

function hasTurnExpectations(scenario: TestScenario): boolean {
  const turns =
    scenario.input.type === 'conversation'
      ? (scenario.input.turns ?? [])
      : scenario.input.type === 'barge_in'
        ? [scenario.input.initial, scenario.input.follow_up].filter((t): t is ConversationTurn => t != null)
        : [];
  return turns.some(t => t.expected != null);
}

function ExpectedField({ label, value, isDark }: { label: string; value: string; isDark: boolean }) {
  return (
    <div className="flex items-start gap-2 text-xs">
      <span className={`font-mono shrink-0 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{label}:</span>
      <span className={`font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>{value}</span>
    </div>
  );
}

function ConversationTurnList({ turns, isDark }: { turns: ConversationTurn[]; isDark: boolean }) {
  return (
    <ol className="mt-1 space-y-1 list-decimal pl-5 text-xs">
      {turns.map(t => (
        <li key={t.id}>
          <span className={`font-mono ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{t.id}</span>{' '}
          <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{t.input.type}</span>
          {t.input.type === 'text' && t.input.text && (
            <span className={isDark ? 'text-slate-300 italic' : 'text-gray-700 italic'}>"{t.input.text}"</span>
          )}
          {t.input.type === 'audio' && t.input.path && (
            <span className={`font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>{t.input.path}</span>
          )}
          <TurnExpectedBadges expected={t.expected} isDark={isDark} />
        </li>
      ))}
    </ol>
  );
}

// Per-turn assertions (evaluated by the runner as turn-level
// `expectation_failures`) rendered inline so a scenario with only
// turn-level checks doesn't read as "No expected outcomes defined".
function TurnExpectedBadges({ expected, isDark }: { expected?: TurnExpected; isDark: boolean }) {
  if (!expected) return null;
  const badges: string[] = [];
  if (expected.transcript_contains?.length) {
    badges.push(`reply contains ${expected.transcript_contains.map(s => `"${s}"`).join(', ')}`);
  }
  if (expected.golden_transcript) badges.push(`golden: "${expected.golden_transcript}"`);
  if (expected.max_latency_ms != null) badges.push(`≤${expected.max_latency_ms}ms`);
  if (badges.length === 0) return null;
  return (
    <>
      {badges.map(b => (
        <span
          key={b}
          className={`ml-1.5 px-1 rounded font-mono text-[10px] ${
            isDark ? 'bg-slate-800 text-cyan-300' : 'bg-cyan-50 text-cyan-700'
          }`}
        >
          {b}
        </span>
      ))}
    </>
  );
}

function BargeInPreview({
  initial, interrupt, followUp, isDark,
}: {
  initial?: ConversationTurn;
  interrupt?: InterruptSpec;
  followUp?: ConversationTurn;
  isDark: boolean;
}) {
  return (
    <div className="mt-1 space-y-1 text-xs">
      {initial && (
        <div>
          <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>initial</span>
          <span className={isDark ? 'text-slate-300' : 'text-gray-700'}>
            {initial.input.type === 'text' ? `"${initial.input.text ?? ''}"` : (initial.input.path ?? '')}
          </span>
        </div>
      )}
      {interrupt && (
        <div>
          <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>interrupt</span>
          <span className={`font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
            mode={interrupt.mode.kind}
            {interrupt.mode.kind === 'inject_audio' && interrupt.mode.path ? `(${interrupt.mode.path})` : ''}
            {interrupt.mode.kind === 'inject_text' && interrupt.mode.text ? `("${interrupt.mode.text}")` : ''}
            {' '}fire_on={interrupt.fire_on.kind}
            {interrupt.fire_on.kind === 'playback_offset' && interrupt.fire_on.offset_ms != null
              ? `(${interrupt.fire_on.offset_ms}ms)`
              : ''}
          </span>
        </div>
      )}
      {followUp && (
        <div>
          <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>follow_up</span>
          <span className={isDark ? 'text-slate-300' : 'text-gray-700'}>
            {followUp.input.type === 'text' ? `"${followUp.input.text ?? ''}"` : (followUp.input.path ?? '')}
          </span>
        </div>
      )}
    </div>
  );
}

function SyntheticCallerPreview({ config, isDark }: { config: SyntheticCallerConfig; isDark: boolean }) {
  const c = config.caller;
  const callerLabel = c.kind === 'mock'
    ? `mock (${c.script ?? '?'})`
    : c.kind === 'openai_realtime'
    ? `openai_realtime (model=${c.model ?? 'default'}, voice=${c.voice ?? 'default'})`
    : `gemini_live (model=${c.model ?? 'default'})`;
  return (
    <div className="mt-1 space-y-1 text-xs">
      <div>
        <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>caller</span>
        <span className={`font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>{callerLabel}</span>
      </div>
      <div>
        <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>persona</span>
        {config.persona_template && (
          <span className={`font-mono mr-2 px-1 rounded ${isDark ? 'bg-slate-800 text-amber-300' : 'bg-amber-100 text-amber-800'}`}>{config.persona_template}</span>
        )}
        <span className={isDark ? 'text-slate-300 italic' : 'text-gray-700 italic'}>"{config.persona}"</span>
      </div>
      <div>
        <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>bridge</span>
        <span className={`font-mono ${(config.bridge ?? 'text') === 'audio'
          ? (isDark ? 'text-emerald-300' : 'text-emerald-700')
          : (isDark ? 'text-slate-300' : 'text-gray-700')}`}>{config.bridge ?? 'text'}</span>
      </div>
      <div>
        <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>stop</span>
        <span className={`font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
          max_turns={config.stop.max_turns}, max_duration={config.stop.max_duration_s}s
          {config.stop.max_caller_silence_s != null ? `, silence≤${config.stop.max_caller_silence_s}s` : ''}
        </span>
      </div>
      {config.barge_in && (
        <div>
          <span className={`uppercase text-[10px] mr-1 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>barge_in</span>
          <span className={`font-mono ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
            every {config.barge_in.every_n_turns} turns
            {config.barge_in.max_consecutive_failures
              ? `, fail-cap=${config.barge_in.max_consecutive_failures}`
              : ''}
          </span>
        </div>
      )}
    </div>
  );
}

function buildExpectedBrief(scenario: TestScenario): string | null {
  const parts: string[] = [];
  const e = scenario.expected;
  if (!e) return null;
  if (e.transcript_contains?.length) parts.push(`contains: ${e.transcript_contains.join(', ')}`);
  if (e.response_intent) parts.push(`intent: ${e.response_intent}`);
  if (e.max_latency_ms != null) parts.push(`max: ${e.max_latency_ms}ms`);
  if (e.must_interrupt) parts.push('must_interrupt');
  if (e.turns_completed != null) parts.push(`turns≥${e.turns_completed}`);
  if (e.min_completed_turns != null) parts.push(`min_turns≥${e.min_completed_turns}`);
  return parts.length > 0 ? parts.join(' · ') : null;
}
