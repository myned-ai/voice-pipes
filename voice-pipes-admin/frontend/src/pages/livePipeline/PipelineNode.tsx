import { Handle, Position } from 'reactflow';
import type { NodeProps } from 'reactflow';
import { Mic, Activity, Speech, FileText, Volume2, Zap, BrainCircuit, AudioLines, Gauge, Edit2, Timer, Sparkles } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { ParamDescriptor } from '../../lib/orchestrator';
import { EditableParam } from './EditableParam';
import { flattenMetadata, isParamVisible, metaTooltip } from './params';
import type { PipelineNodeData } from './types';

// 1. Icon mapping for pipeline components
const iconMap: Record<string, LucideIcon> = {
  input: Mic,
  vad: Zap,
  stt: FileText,
  turn_detector: Timer,
  llm: BrainCircuit,
  tts: Speech,
  resample: AudioLines,
  output: Volume2,
  core: Activity,
  // Speech-to-Speech (Realtime) provider. Rendered as a single
  // "brain + audio" node because the remote model owns STT + LLM +
  // TTS internally — no separate components exist in this topology.
  realtime: BrainCircuit,
  // Audio post-processor ([[audio_post_processor]]): enriches output audio
  // chunks (e.g. blendshapes via the gRPC post service) before transport.
  post: Sparkles,
};

// 2. Build a highly styled Custom Node
export function CustomPipelineNode({ data }: NodeProps<PipelineNodeData>) {
  const { isDark } = useTheme();
  const Icon = iconMap[data.componentType] || Activity;
  const metricKeys = data.metrics ? Object.keys(data.metrics) : [];
  // Pulled out so the narrowing survives into the `onSubmit` closure below.
  const onParamUpdate = data.onParamUpdate;

  // ── Schema-driven param rendering ─────────────────────────────────────────
  // `param_schema` comes from the server. Each entry knows its label,
  // description, visibility condition, and mutability. The frontend evaluates
  // `only_when` against the live metadata values and renders accordingly.
  const paramSchema: ParamDescriptor[] = data.param_schema || [];
  const metadata: Record<string, unknown> = data.metadata || {};

  // Visible schema-declared params (only_when filter applied)
  const visibleSchemaParams = paramSchema.filter(p => isParamVisible(p, metadata));
  const schematedKeys = new Set(paramSchema.map(p => p.key));

  // Params present in metadata but not declared in schema (e.g. unknown options)
  // — show them as plain unlabelled entries so no info is lost.
  const extraRows = flattenMetadata(metadata, schematedKeys);

  const hasConfig = visibleSchemaParams.length > 0 || extraRows.length > 0;
  
  const isModelNode = ['stt', 'turn_detector', 'llm', 'tts', 'realtime'].includes(data.componentType);
  const isInputNode = data.componentType === 'input';
  const isOutputNode = data.componentType === 'output';
  const isSpecial = isModelNode || isInputNode || isOutputNode;

  
  // Standard structural nodes
  const bgStyleNormal = isDark ? '#1e293b' : '#ffffff';
  const borderNormal = isDark ? '1px solid #334155' : '1px solid #e5e7eb';
  const textNormal = isDark ? '#f8fafc' : '#1f2937';
  const subTextNormal = isDark ? '#94a3b8' : '#6b7280';
  const iconBgNormal = isDark ? '#334155' : '#eff6ff';
  
  // Styling overrides based on component focus
  let gradientLight = 'linear-gradient(135deg, #818cf8, #6366f1)';
  let gradientDark = 'linear-gradient(135deg, #4f46e5, #3730a3)';
  let accentMain = isDark ? '#818cf8' : '#a5b4fc';
  let accentSub = '#e0e7ff';
  let iconBgDark = '#312e81';
  let iconBgLight = '#4f46e5';
  let badgeText = 'AI';
  let badgeBg = isDark ? '#312e81' : '#4338ca';
  let shadowRgb = '99, 102, 241';

  if (isInputNode) {
    gradientLight = 'linear-gradient(135deg, #10b981, #059669)'; 
    gradientDark = 'linear-gradient(135deg, #059669, #064e3b)';
    accentMain = isDark ? '#34d399' : '#6ee7b7';
    accentSub = '#d1fae5';
    iconBgDark = '#065f46';
    iconBgLight = '#047857';
    badgeText = 'INPUT';
    badgeBg = isDark ? '#065f46' : '#047857';
    shadowRgb = '16, 185, 129';
  } else if (isOutputNode) {
    gradientLight = 'linear-gradient(135deg, #f97316, #ea580c)'; 
    gradientDark = 'linear-gradient(135deg, #ea580c, #9a3412)';
    accentMain = isDark ? '#fb923c' : '#fdba74';
    accentSub = '#ffedd5';
    iconBgDark = '#7c2d12';
    iconBgLight = '#c2410c';
    badgeText = 'OUTPUT';
    badgeBg = isDark ? '#7c2d12' : '#c2410c';
    shadowRgb = '249, 115, 22';
  }

  const bgStyle = isSpecial ? (isDark ? gradientDark : gradientLight) : bgStyleNormal;
  const borderStyle = isSpecial ? `2px solid ${accentMain}` : borderNormal;
  const shadowStyle = isSpecial 
      ? `0 10px 25px -5px rgba(${shadowRgb}, ${isDark ? '0.2' : '0.4'})` 
      : (isDark ? '0 10px 15px -3px rgba(0, 0, 0, 0.4)' : '0 10px 15px -3px rgba(0, 0, 0, 0.1)');
      
  const textColor = isSpecial ? '#ffffff' : textNormal;
  const subTextColor = isSpecial ? accentSub : subTextNormal;
  const iconBg = isSpecial ? (isDark ? iconBgDark : iconBgLight) : iconBgNormal;
  const iconColor = isSpecial ? '#ffffff' : '#3b82f6';
  
  return (
    <div style={{
      padding: '16px',
      borderRadius: '16px',
      background: bgStyle,
      border: borderStyle,
      boxShadow: shadowStyle,
      display: 'flex',
      flexDirection: 'column',
      gap: '12px',
      minWidth: '260px',
      fontFamily: 'system-ui, sans-serif',
      transition: 'all 0.3s ease-in-out'
    }}>
      {/* Input Handle */}
      {data.componentType !== 'input' && (
        <Handle type="target" position={Position.Left} style={{ width: 12, height: 12, background: isSpecial ? accentMain : '#3b82f6', border: '2px solid white' }} />
      )}
      
      <div style={{ display: 'flex', alignItems: 'center', gap: '16px' }}>
        <div style={{ background: iconBg, padding: '12px', borderRadius: '12px', color: iconColor, display: 'flex', flexShrink: 0 }}>
          <Icon size={24} strokeWidth={2.5} />
        </div>
        <div style={{ flex: 1, overflow: 'hidden' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
            <div style={{ fontSize: '16px', fontWeight: 600, color: textColor, whiteSpace: 'nowrap', textOverflow: 'ellipsis' }}>{data.label}</div>
            {isSpecial && (
               <span style={{ background: badgeBg, color: '#fff', fontSize: '9px', padding: '2px 6px', borderRadius: '6px', fontWeight: 700, letterSpacing: '0.05em' }}>{badgeText}</span>
            )}
          </div>
          {data.provider && <div style={{ fontSize: '13px', color: subTextColor, marginTop: '2px' }}>Provider: <span style={{ fontWeight: 500, color: isSpecial ? accentMain : 'inherit' }}>{data.provider}</span></div>}
        </div>
      </div>

      {/* Sample rate badges */}
      {(data.input_sample_rate || data.output_sample_rate) && (
        <div style={{ display: 'flex', gap: '6px', flexWrap: 'wrap' }}>
          {data.input_sample_rate && (
            <span style={{
              fontSize: '10px', fontWeight: 600, fontFamily: 'monospace',
              padding: '2px 8px', borderRadius: '6px',
              background: isSpecial ? 'rgba(0,0,0,0.2)' : (isDark ? '#1e293b' : '#f1f5f9'),
              color: isSpecial ? accentSub : (isDark ? '#94a3b8' : '#64748b'),
              border: isSpecial ? '1px solid rgba(255,255,255,0.15)' : (isDark ? '1px solid #334155' : '1px solid #e2e8f0'),
            }}>IN {(data.input_sample_rate / 1000).toFixed(0)} kHz</span>
          )}
          {data.output_sample_rate && (
            <span style={{
              fontSize: '10px', fontWeight: 600, fontFamily: 'monospace',
              padding: '2px 8px', borderRadius: '6px',
              background: isSpecial ? 'rgba(0,0,0,0.2)' : (isDark ? '#1e293b' : '#f1f5f9'),
              color: isSpecial ? accentSub : (isDark ? '#94a3b8' : '#64748b'),
              border: isSpecial ? '1px solid rgba(255,255,255,0.15)' : (isDark ? '1px solid #334155' : '1px solid #e2e8f0'),
            }}>OUT {(data.output_sample_rate / 1000).toFixed(0)} kHz</span>
          )}
        </div>
      )}

      {hasConfig && (
        <div style={{ 
          background: isSpecial ? 'rgba(0,0,0,0.15)' : (isDark ? '#0f172a' : '#f8fafc'), 
          border: isSpecial ? '1px solid rgba(255,255,255,0.2)' : (isDark ? '1px solid #1e293b' : '1px solid #e2e8f0'), 
          borderRadius: '8px', 
          padding: '10px 12px',
          display: 'flex',
          flexDirection: 'column',
          gap: '8px'
        }}>
          <div style={{ fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.05em', color: isSpecial ? accentMain : (isDark ? '#64748b' : '#94a3b8'), fontWeight: 700 }}>Configuration</div>

          {/* Schema-declared params — filtered by only_when, labelled, with mutable indicator */}
          {visibleSchemaParams.map(param => {
            const val = metadata[param.key];
            if (val == null || val === '') return null;
            const valueColor = isSpecial ? '#ffffff' : (isDark ? '#f8fafc' : '#0f172a');
            return (
              <div key={param.key} style={{ display: 'flex', justifyContent: 'space-between', fontSize: '12px', gap: '12px', alignItems: 'center' }}>
                <span
                  title={param.description}
                  style={{
                    color: isSpecial ? accentSub : (isDark ? '#94a3b8' : '#64748b'),
                    cursor: 'help',
                    borderBottom: '1px dotted currentColor',
                    display: 'flex',
                    alignItems: 'center',
                    gap: '4px',
                    flexShrink: 0,
                  }}
                >
                  {param.label}
                  {param.mutable && (
                    <span title="Can be changed at runtime without restarting the worker" style={{ display: 'inline-flex', alignItems: 'center' }}>
                      <Edit2 size={9} style={{ opacity: 0.6 }} />
                    </span>
                  )}
                </span>
                {param.mutable && onParamUpdate ? (
                  <EditableParam
                    param={param}
                    value={val}
                    displayColor={valueColor}
                    accentColor={isSpecial ? '#ffffff' : '#3b82f6'}
                    inputBg={isDark ? '#0f172a' : '#ffffff'}
                    inputBorder={isDark ? '#334155' : '#cbd5e1'}
                    errorColor="#ef4444"
                    disabled={!data.workerId}
                    onSubmit={(newValue) => onParamUpdate(data.nodeId, param.key, newValue)}
                  />
                ) : (
                  <span style={{
                    color: valueColor,
                    fontWeight: 500,
                    fontFamily: 'monospace',
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                    maxWidth: '140px',
                    textAlign: 'right'
                  }} title={String(val)}>{String(val)}</span>
                )}
              </div>
            );
          })}

          {/* Extra params present in metadata but not declared in schema */}
          {extraRows.map(([label, display, raw]) => (
            <div key={label} style={{ display: 'flex', justifyContent: 'space-between', fontSize: '12px', gap: '12px' }}>
              <span style={{ color: isSpecial ? accentSub : (isDark ? '#94a3b8' : '#64748b') }}>
                {label.replace(/_/g, ' ')}
              </span>
              <span style={{
                 color: isSpecial ? '#ffffff' : (isDark ? '#f8fafc' : '#0f172a'),
                 fontWeight: 500,
                 fontFamily: 'monospace',
                 overflow: 'hidden',
                 textOverflow: 'ellipsis',
                 whiteSpace: 'nowrap',
                 maxWidth: '140px',
                 textAlign: 'right'
              }} title={metaTooltip(raw)}>{display}</span>
            </div>
          ))}
        </div>
      )}

      {data.showMetrics && metricKeys.length > 0 && (
        <div style={{ 
          background: isSpecial ? 'rgba(255,255,255,0.15)' : (isDark ? '#1e293b' : '#eff6ff'), 
          border: isSpecial ? `1px solid rgba(255,255,255,0.4)` : (isDark ? '1px solid #3b82f6' : '1px solid #bfdbfe'), 
          borderRadius: '8px', 
          padding: '10px 12px',
          display: 'flex',
          flexDirection: 'column',
          gap: '8px'
        }}>
          <div style={{ fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.05em', color: isSpecial ? '#ffffff' : '#3b82f6', fontWeight: 700, display: 'flex', alignItems: 'center', gap: '4px' }}>
            <Gauge size={12} />
            Live Metrics (last 60s)
          </div>
          {metricKeys.map(key => (
            <div key={key} style={{ display: 'flex', justifyContent: 'space-between', fontSize: '12px', gap: '12px' }}>
              <span style={{ color: isSpecial ? accentSub : (isDark ? '#94a3b8' : '#64748b') }}>{key.replace(/_/g, ' ')}</span>
              <span style={{ 
                 color: isSpecial ? '#ffffff' : (isDark ? '#f8fafc' : '#0f172a'), 
                 fontWeight: 600, 
                 fontFamily: 'monospace', 
                 overflow: 'hidden', 
                 textOverflow: 'ellipsis', 
                 whiteSpace: 'nowrap', 
                 maxWidth: '140px', 
                 textAlign: 'right' 
              }}>{String(data.metrics[key])}</span>
            </div>
          ))}
        </div>
      )}

      {/* Output Handle */}
      {data.componentType !== 'output' && (
        <Handle type="source" position={Position.Right} style={{ width: 12, height: 12, background: isSpecial ? accentMain : '#3b82f6', border: '2px solid white' }} />
      )}
    </div>
  );
}
