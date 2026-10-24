import { AreaChart, Area, LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, ReferenceLine, Legend } from 'recharts';
import { GLOBAL_SLO } from '../../lib/slo';
import { ChangeLines, ChartTooltip, LlmTooltip } from './chartParts';
import type { AnalyticsData } from './types';
import type { DashboardMetrics } from './useDashboardMetrics';

interface EndToEndLatencyChartProps {
  isDark: boolean;
  data: AnalyticsData;
  handleChartClick: (e: unknown) => void;
  chartData: DashboardMetrics['chartData'];
  e2eYDomain: DashboardMetrics['e2eYDomain'];
  axisStroke: string;
  tickFill: string;
  gridStroke: string;
}

/** End-to-end latency per turn. */
export function EndToEndLatencyChart({ isDark, data, handleChartClick, chartData, e2eYDomain, axisStroke, tickFill, gridStroke }: EndToEndLatencyChartProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="flex items-center justify-between mb-4">
        <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
          End-to-End Latency
        </h3>
        <span className={`text-[11px] italic ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          tip: click any point to open the session
        </span>
      </div>
      <ResponsiveContainer width="100%" height={280}>
        <AreaChart data={chartData} onClick={handleChartClick} style={{ cursor: 'pointer' }}>
          <defs>
            <linearGradient id="gradTotal" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#6366f1" stopOpacity={0.3} />
              <stop offset="95%" stopColor="#6366f1" stopOpacity={0} />
            </linearGradient>
            <linearGradient id="gradTtfb" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#f59e0b" stopOpacity={0.2} />
              <stop offset="95%" stopColor="#f59e0b" stopOpacity={0} />
            </linearGradient>
            <linearGradient id="gradTtft" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#10b981" stopOpacity={0.2} />
              <stop offset="95%" stopColor="#10b981" stopOpacity={0} />
            </linearGradient>
            <linearGradient id="gradEp" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#f43f5e" stopOpacity={0.2} />
              <stop offset="95%" stopColor="#f43f5e" stopOpacity={0} />
            </linearGradient>
          </defs>
          <CartesianGrid strokeDasharray="3 3" stroke={gridStroke} />
          <XAxis
            dataKey="_time"
            type="number"
            domain={['dataMin', 'dataMax']}
            tickFormatter={(v) => new Date(v).toLocaleDateString()}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <YAxis
            domain={[0, e2eYDomain.yMax]}
            allowDataOverflow
            tickFormatter={(v) => `${(v / 1000).toFixed(1)}s`}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <Tooltip content={<ChartTooltip isDark={isDark} />} />
          <Legend />
          <ReferenceLine
            y={GLOBAL_SLO.total_p95_ms}
            stroke="#f43f5e"
            strokeDasharray="4 4"
            label={{
              value: `p95 SLO ${(GLOBAL_SLO.total_p95_ms / 1000).toFixed(1)}s`,
              position: 'insideTopRight',
              fill: isDark ? '#fda4af' : '#be123c',
              fontSize: 10,
            }}
            ifOverflow="hidden"
          />
          <ChangeLines changes={data.provider_changes} dataKey="latency" />
          <Area
            type="monotone"
            dataKey="total_ms"
            name="Total Latency"
            stroke="#6366f1"
            fill="url(#gradTotal)"
            strokeWidth={2}
            dot={false}
            connectNulls
          />
          <Area
            type="monotone"
            dataKey="ttfb_ms"
            name="Time to First Byte"
            stroke="#f59e0b"
            fill="url(#gradTtfb)"
            strokeWidth={1.5}
            dot={false}
            connectNulls
          />
          <Area
            type="monotone"
            dataKey="ttft_ms"
            name="Time to First Token"
            stroke="#10b981"
            fill="url(#gradTtft)"
            strokeWidth={1.5}
            dot={false}
            connectNulls
          />
          <Area
            type="monotone"
            dataKey="endpointing_ms"
            name="Endpointing Delay"
            stroke="#f43f5e"
            fill="url(#gradEp)"
            strokeWidth={1.5}
            dot={false}
            connectNulls
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}

interface LlmPerformanceChartProps {
  isDark: boolean;
  data: AnalyticsData;
  handleChartClick: (e: unknown) => void;
  chartData: DashboardMetrics['chartData'];
  axisStroke: string;
  tickFill: string;
  gridStroke: string;
}

/** LLM TTFT and throughput per turn. */
export function LlmPerformanceChart({ isDark, data, handleChartClick, chartData, axisStroke, tickFill, gridStroke }: LlmPerformanceChartProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h3 className={`text-sm font-semibold mb-4 ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
        LLM Performance
      </h3>
      <ResponsiveContainer width="100%" height={280}>
        <LineChart data={chartData} onClick={handleChartClick} style={{ cursor: 'pointer' }}>
          <CartesianGrid strokeDasharray="3 3" stroke={gridStroke} />
          <XAxis
            dataKey="_time"
            type="number"
            domain={['dataMin', 'dataMax']}
            tickFormatter={(v) => new Date(v).toLocaleDateString()}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <YAxis
            yAxisId="left"
            label={{ value: 'tok/s', angle: -90, position: 'insideLeft', fill: tickFill, fontSize: 11 }}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <YAxis
            yAxisId="right"
            orientation="right"
            label={{ value: 'ms', angle: 90, position: 'insideRight', fill: tickFill, fontSize: 11 }}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <Tooltip content={<LlmTooltip isDark={isDark} />} />
          <Legend />
          <ReferenceLine
            yAxisId="right"
            y={GLOBAL_SLO.ttft_p95_ms}
            stroke="#f43f5e"
            strokeDasharray="4 4"
            label={{
              value: `TTFT SLO ${GLOBAL_SLO.ttft_p95_ms}ms`,
              position: 'insideTopRight',
              fill: isDark ? '#fda4af' : '#be123c',
              fontSize: 10,
            }}
            ifOverflow="extendDomain"
          />
          <ChangeLines changes={data.provider_changes} dataKey="llm" />
          <Line
            yAxisId="left"
            type="monotone"
            dataKey="tokens_per_sec"
            name="Tokens/sec"
            stroke="#06b6d4"
            strokeWidth={2}
            dot={false}
            connectNulls
          />
          <Line
            yAxisId="right"
            type="monotone"
            dataKey="first_token_ms"
            name="First Token"
            stroke="#f97316"
            strokeWidth={2}
            dot={false}
            connectNulls
          />
          <Line
            yAxisId="right"
            type="monotone"
            dataKey="generate_ms"
            name="Generate Time"
            stroke="#a78bfa"
            strokeWidth={1.5}
            strokeDasharray="4 2"
            dot={false}
            connectNulls
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}

interface TokenUsageChartProps {
  isDark: boolean;
  data: AnalyticsData;
  handleChartClick: (e: unknown) => void;
  chartData: DashboardMetrics['chartData'];
  axisStroke: string;
  tickFill: string;
  gridStroke: string;
}

/** Prompt and output tokens per turn. */
export function TokenUsageChart({ isDark, data, handleChartClick, chartData, axisStroke, tickFill, gridStroke }: TokenUsageChartProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h3 className={`text-sm font-semibold mb-4 ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
        Token Usage
      </h3>
      <ResponsiveContainer width="100%" height={220}>
        <AreaChart data={chartData} onClick={handleChartClick} style={{ cursor: 'pointer' }}>
          <defs>
            <linearGradient id="gradPrompt" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#3b82f6" stopOpacity={0.3} />
              <stop offset="95%" stopColor="#3b82f6" stopOpacity={0} />
            </linearGradient>
            <linearGradient id="gradOutput" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#10b981" stopOpacity={0.3} />
              <stop offset="95%" stopColor="#10b981" stopOpacity={0} />
            </linearGradient>
          </defs>
          <CartesianGrid strokeDasharray="3 3" stroke={gridStroke} />
          <XAxis
            dataKey="_time"
            type="number"
            domain={['dataMin', 'dataMax']}
            tickFormatter={(v) => new Date(v).toLocaleDateString()}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <YAxis
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
            label={{ value: 'tokens', angle: -90, position: 'insideLeft', fill: tickFill, fontSize: 11 }}
          />
          <Tooltip content={<ChartTooltip isDark={isDark} />} />
          <Legend />
          <ChangeLines changes={data.provider_changes} dataKey="tokens" />
          <Area
            type="monotone"
            dataKey="prompt_tokens"
            name="Prompt Tokens"
            stroke="#3b82f6"
            fill="url(#gradPrompt)"
            strokeWidth={1.5}
            dot={false}
            connectNulls
          />
          <Area
            type="monotone"
            dataKey="output_tokens"
            name="Output Tokens"
            stroke="#10b981"
            fill="url(#gradOutput)"
            strokeWidth={1.5}
            dot={false}
            connectNulls
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}
