import type React from 'react';
import { AreaChart, Area, LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, ReferenceLine, Legend } from 'recharts';
import { pipelineColor, pipelineColorAlpha } from '../../lib/pipelineColor';
import { GLOBAL_SLO } from '../../lib/slo';
import { fmtMs, fmtUsd } from './format';
import type { Agg, Bucket } from './types';
import type { DashboardMetrics } from './useDashboardMetrics';

interface LatencyByPipelineChartProps {
  isDark: boolean;
  bucket: Bucket;
  setBucket: React.Dispatch<React.SetStateAction<Bucket>>;
  agg: Agg;
  setAgg: React.Dispatch<React.SetStateAction<Agg>>;
  bucketedSeries: DashboardMetrics['bucketedSeries'];
  axisStroke: string;
  tickFill: string;
  gridStroke: string;
}

/** Time-bucketed latency per pipeline. */
export function LatencyByPipelineChart({ isDark, bucket, setBucket, agg, setAgg, bucketedSeries, axisStroke, tickFill, gridStroke }: LatencyByPipelineChartProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="flex items-center justify-between mb-4 gap-3 flex-wrap">
        <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
          Latency by pipeline (bucketed)
        </h3>
        <div className="flex items-center gap-2 text-xs">
          <span className={isDark ? 'text-slate-500' : 'text-gray-500'}>bucket</span>
          <select
            value={bucket}
            onChange={(e) => setBucket(e.target.value as typeof bucket)}
            className={`rounded border px-2 py-0.5 ${isDark ? 'bg-slate-900 border-slate-700 text-slate-200' : 'bg-white border-gray-300 text-gray-700'}`}
          >
            <option value="auto">auto</option>
            <option value="1m">1 min</option>
            <option value="5m">5 min</option>
            <option value="1h">1 h</option>
            <option value="1d">1 d</option>
          </select>
          <span className={isDark ? 'text-slate-500' : 'text-gray-500'}>agg</span>
          <select
            value={agg}
            onChange={(e) => setAgg(e.target.value as typeof agg)}
            className={`rounded border px-2 py-0.5 ${isDark ? 'bg-slate-900 border-slate-700 text-slate-200' : 'bg-white border-gray-300 text-gray-700'}`}
          >
            <option value="avg">avg</option>
            <option value="p50">p50</option>
            <option value="p95">p95</option>
            <option value="p99">p99</option>
            <option value="max">max</option>
          </select>
        </div>
      </div>
      <ResponsiveContainer width="100%" height={280}>
        <LineChart data={bucketedSeries.rows}>
          <CartesianGrid strokeDasharray="3 3" stroke={gridStroke} />
          <XAxis
            dataKey="_time"
            type="number"
            domain={['dataMin', 'dataMax']}
            tickFormatter={(v) => new Date(v).toLocaleString()}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <YAxis
            domain={[0, bucketedSeries.yMax]}
            allowDataOverflow
            tickFormatter={(v) => `${(v / 1000).toFixed(1)}s`}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <Tooltip
            labelFormatter={(v) => new Date(v as number).toLocaleString()}
            formatter={(value, name) => [fmtMs(Number(value)), String(name)]}
            contentStyle={{
              background: isDark ? '#0f172a' : '#ffffff',
              border: `1px solid ${isDark ? '#334155' : '#e5e7eb'}`,
              borderRadius: 8,
              color: isDark ? '#e2e8f0' : '#111827',
            }}
          />
          <Legend />
          <ReferenceLine
            y={GLOBAL_SLO.total_p95_ms}
            stroke="#f43f5e"
            strokeDasharray="4 4"
            label={{
              value: `SLO ${(GLOBAL_SLO.total_p95_ms / 1000).toFixed(1)}s`,
              position: 'insideTopRight',
              fill: isDark ? '#fda4af' : '#be123c',
              fontSize: 10,
            }}
            ifOverflow="hidden"
          />
          {bucketedSeries.pipelines.map((name) => (
            <Line
              key={name}
              type="monotone"
              dataKey={name}
              name={name}
              stroke={pipelineColor(name)}
              strokeWidth={2}
              dot={false}
              connectNulls
            />
          ))}
        </LineChart>
      </ResponsiveContainer>
      {bucketedSeries.outliers > 0 && (
        <p className={`text-xs mt-2 text-right ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
          ↑ {bucketedSeries.outliers} bucket{bucketedSeries.outliers === 1 ? '' : 's'} above {(bucketedSeries.yMax / 1000).toFixed(1)}s clipped for readability
        </p>
      )}
    </div>
  );
}

interface CostByPipelineChartProps {
  isDark: boolean;
  bucketedCost: DashboardMetrics['bucketedCost'];
  axisStroke: string;
  tickFill: string;
  gridStroke: string;
}

/** Time-bucketed LLM cost per pipeline. */
export function CostByPipelineChart({ isDark, bucketedCost, axisStroke, tickFill, gridStroke }: CostByPipelineChartProps) {
  return (
    <div className={`rounded-xl p-5 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="flex items-center justify-between mb-4 gap-3 flex-wrap">
        <h3 className={`text-sm font-semibold ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>
          LLM cost by pipeline (bucketed · sum)
        </h3>
        <span className={`text-xs ${isDark ? 'text-slate-500' : 'text-gray-500'}`}>
          bucket follows the latency chart selector
        </span>
      </div>
      <ResponsiveContainer width="100%" height={260}>
        <AreaChart data={bucketedCost.rows}>
          <CartesianGrid strokeDasharray="3 3" stroke={gridStroke} />
          <XAxis
            dataKey="_time"
            type="number"
            domain={['dataMin', 'dataMax']}
            tickFormatter={(v) => new Date(v).toLocaleString()}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <YAxis
            tickFormatter={(v) => fmtUsd(v)}
            stroke={axisStroke}
            tick={{ fill: tickFill, fontSize: 11 }}
          />
          <Tooltip
            labelFormatter={(v) => new Date(v as number).toLocaleString()}
            formatter={(value, name) => [fmtUsd(Number(value)), String(name)]}
            contentStyle={{
              background: isDark ? '#0f172a' : '#ffffff',
              border: `1px solid ${isDark ? '#334155' : '#e5e7eb'}`,
              borderRadius: 8,
              color: isDark ? '#e2e8f0' : '#111827',
            }}
          />
          <Legend />
          {bucketedCost.pipelines.map((name) => (
            <Area
              key={name}
              type="monotone"
              dataKey={name}
              name={name}
              stackId="cost"
              stroke={pipelineColor(name)}
              fill={pipelineColorAlpha(name, 0.55)}
            />
          ))}
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}
