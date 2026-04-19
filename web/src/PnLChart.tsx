import { useEffect, useRef, useState, useCallback } from 'react';

interface PnlPoint {
  timestamp: string;
  equity: number;
  pnl: number;
}

type TimeRange = '1H' | '1D' | '7D';

const PADDING = { top: 20, right: 60, bottom: 40, left: 60 };

function formatTime(ts: string, range: TimeRange): string {
  const d = new Date(ts);
  if (range === '1H') return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (range === '1D') return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

function formatValue(v: number): string {
  if (Math.abs(v) >= 1000) return `$${(v / 1000).toFixed(1)}k`;
  return `$${v.toFixed(2)}`;
}

export default function PnLChart() {
  const [data, setData] = useState<PnlPoint[]>([]);
  const [range, setRange] = useState<TimeRange>('1D');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [hover, setHover] = useState<{ x: number; point: PnlPoint } | null>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  const fetchData = useCallback(async () => {
    try {
      const res = await fetch('/api/pnl_history');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      setData(Array.isArray(json) ? json : []);
      setError('');
    } catch (e: any) {
      setError('Failed to load PnL data. Ensure the daemon is running.');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchData();
    const iv = setInterval(fetchData, 10000);
    return () => clearInterval(iv);
  }, [fetchData]);

  const now = Date.now();
  const windowMs: Record<TimeRange, number> = {
    '1H': 60 * 60 * 1000,
    '1D': 24 * 60 * 60 * 1000,
    '7D': 7 * 24 * 60 * 60 * 1000,
  };

  const filtered = data.filter(p => {
    const age = now - new Date(p.timestamp).getTime();
    return age <= windowMs[range];
  });

  if (loading) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 300, color: 'var(--text-secondary)' }}>
        Loading PnL history...
      </div>
    );
  }

  if (error) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: 300, color: '#f87171', fontSize: '0.875rem' }}>
        {error}
      </div>
    );
  }

  if (filtered.length === 0) {
    return (
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', height: 300, gap: '0.5rem' }}>
        <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="var(--text-secondary)" strokeWidth="1.5">
          <path d="M3 3v18h18" /><path d="M7 16l4-4 4 4 5-6" />
        </svg>
        <span style={{ color: 'var(--text-secondary)', fontSize: '0.875rem' }}>
          No data for {range}. Bot may be idle.
        </span>
        <span style={{ color: 'var(--text-secondary)', fontSize: '0.75rem' }}>
          {data.length} total snapshots stored
        </span>
      </div>
    );
  }

  const W = 700;
  const H = 300;
  const chartW = W - PADDING.left - PADDING.right;
  const chartH = H - PADDING.top - PADDING.bottom;

  const minTs = Math.min(...filtered.map(p => new Date(p.timestamp).getTime()));
  const maxTs = Math.max(...filtered.map(p => new Date(p.timestamp).getTime()));
  const rangeTs = maxTs - minTs || 1;

  const allValues = filtered.flatMap(p => [p.equity, p.pnl]);
  const minVal = Math.min(...allValues);
  const maxVal = Math.max(...allValues);
  const rangeVal = maxVal - minVal || 1;

  const xOf = (ts: string) => PADDING.left + ((new Date(ts).getTime() - minTs) / rangeTs) * chartW;
  const yOf = (v: number) => PADDING.top + chartH - ((v - minVal) / rangeVal) * chartH;

  // Grid lines
  const gridLines = [];
  const numTicks = 5;
  for (let i = 0; i <= numTicks; i++) {
    const val = minVal + (rangeVal * i) / numTicks;
    const y = yOf(val);
    gridLines.push(
      <g key={i}>
        <line x1={PADDING.left} y1={y} x2={W - PADDING.right} y2={y} stroke="rgba(255,255,255,0.06)" strokeWidth="1" />
        <text x={W - PADDING.right + 6} y={y + 4} fontSize="11" fill="var(--text-secondary)" textAnchor="start">
          {formatValue(val)}
        </text>
      </g>
    );
  }

  // X-axis ticks
  const xTicks = [];
  const numXTicks = Math.min(filtered.length, 6);
  for (let i = 0; i <= numXTicks; i++) {
    const idx = Math.floor((i / numXTicks) * (filtered.length - 1));
    const p = filtered[idx];
    if (!p) continue;
    const x = xOf(p.timestamp);
    xTicks.push(
      <text key={i} x={x} y={H - 8} fontSize="11" fill="var(--text-secondary)" textAnchor="middle">
        {formatTime(p.timestamp, range)}
      </text>
    );
  }

  // Equity line path
  const equityPath = filtered.map((p, i) => {
    const x = xOf(p.timestamp);
    const y = yOf(p.equity);
    return `${i === 0 ? 'M' : 'L'} ${x} ${y}`;
  }).join(' ');

  // PnL line path
  const pnlPath = filtered.map((p, i) => {
    const x = xOf(p.timestamp);
    const y = yOf(p.pnl);
    return `${i === 0 ? 'M' : 'L'} ${x} ${y}`;
  }).join(' ');

  // Area fill for equity
  const equityArea = `${equityPath} L ${xOf(filtered[filtered.length - 1].timestamp)} ${yOf(minVal)} L ${xOf(filtered[0].timestamp)} ${yOf(minVal)} Z`;

  // Handle mouse interaction
  const handleMouseMove = (e: React.MouseEvent<SVGSVGElement>) => {
    if (!svgRef.current) return;
    const rect = svgRef.current.getBoundingClientRect();
    const svgX = ((e.clientX - rect.left) / rect.width) * W;
    const relX = svgX - PADDING.left;
    if (relX < 0 || relX > chartW) { setHover(null); return; }
    const ts = minTs + (relX / chartW) * rangeTs;
    let closest = filtered[0];
    let minDist = Infinity;
    for (const p of filtered) {
      const d = Math.abs(new Date(p.timestamp).getTime() - ts);
      if (d < minDist) { minDist = d; closest = p; }
    }
    setHover({ x: xOf(closest.timestamp), point: closest });
  };

  // Legend
  const last = filtered[filtered.length - 1];
  const first = filtered[0];

  return (
    <div ref={containerRef} style={{ width: '100%' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '0.75rem' }}>
        <div style={{ display: 'flex', gap: '1.5rem', fontSize: '0.8rem' }}>
          <span style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
            <svg width="20" height="3"><line x1="0" y1="1.5" x2="20" y2="1.5" stroke="#60a5fa" strokeWidth="2" /></svg>
            <span style={{ color: 'var(--text-secondary)' }}>Equity</span>
            <span style={{ color: '#60a5fa', fontWeight: 600 }}>{formatValue(last?.equity ?? 0)}</span>
          </span>
          <span style={{ display: 'flex', alignItems: 'center', gap: '0.4rem' }}>
            <svg width="20" height="3"><line x1="0" y1="1.5" x2="20" y2="1.5" stroke="#a78bfa" strokeWidth="2" /></svg>
            <span style={{ color: 'var(--text-secondary)' }}>PnL</span>
            <span style={{ color: '#a78bfa', fontWeight: 600 }}>{formatValue(last?.pnl ?? 0)}</span>
          </span>
        </div>
        <div style={{ display: 'flex', gap: '0.25rem' }}>
          {(['1H', '1D', '7D'] as TimeRange[]).map(r => (
            <button
              key={r}
              onClick={() => setRange(r)}
              style={{
                padding: '3px 10px',
                fontSize: '0.75rem',
                fontWeight: 600,
                background: range === r ? 'rgba(96,165,250,0.2)' : 'transparent',
                border: `1px solid ${range === r ? 'rgba(96,165,250,0.5)' : 'var(--border-color)'}`,
                borderRadius: '4px',
                color: range === r ? '#60a5fa' : 'var(--text-secondary)',
                cursor: 'pointer',
                transition: 'all 0.15s',
              }}
            >
              {r}
            </button>
          ))}
        </div>
      </div>

      <svg
        ref={svgRef}
        viewBox={`0 0 ${W} ${H}`}
        style={{ width: '100%', maxWidth: W, height: 'auto', cursor: 'crosshair', display: 'block' }}
        onMouseMove={handleMouseMove}
        onMouseLeave={() => setHover(null)}
      >
        {gridLines}
        {xTicks}

        {/* Equity area */}
        <path d={equityArea} fill="rgba(96,165,250,0.06)" />

        {/* Equity line */}
        <path d={equityPath} fill="none" stroke="#60a5fa" strokeWidth="2" />

        {/* PnL line */}
        <path d={pnlPath} fill="none" stroke="#a78bfa" strokeWidth="1.5" strokeDasharray="4 2" />

        {/* Zero line for PnL */}
        {last && (
          <line
            x1={PADDING.left} y1={yOf(0)} x2={W - PADDING.right} y2={yOf(0)}
            stroke="rgba(255,255,255,0.15)" strokeWidth="1" strokeDasharray="3 3"
          />
        )}

        {/* Hover indicator */}
        {hover && (
          <g>
            <line x1={hover.x} y1={PADDING.top} x2={hover.x} y2={H - PADDING.bottom}
              stroke="rgba(255,255,255,0.2)" strokeWidth="1" />
            <circle cx={hover.x} cy={yOf(hover.point.equity)} r="4" fill="#60a5fa" />
            <circle cx={hover.x} cy={yOf(hover.point.pnl)} r="3" fill="#a78bfa" />

            {/* Tooltip */}
            {(() => {
              const tx = hover.x + 10 > W - PADDING.right - 160 ? hover.x - 170 : hover.x + 10;
              const ty = Math.max(PADDING.top, yOf(hover.point.equity) - 60);
              return (
                <g>
                  <rect x={tx} y={ty} width="160" height="56" rx="6"
                    fill="rgba(15,20,35,0.95)" stroke="rgba(255,255,255,0.15)" strokeWidth="1" />
                  <text x={tx + 8} y={ty + 16} fontSize="11" fill="var(--text-secondary)">
                    {new Date(hover.point.timestamp).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}
                  </text>
                  <text x={tx + 8} y={ty + 32} fontSize="12" fill="#60a5fa" fontWeight={600}>
                    Equity {formatValue(hover.point.equity)}
                  </text>
                  <text x={tx + 8} y={ty + 48} fontSize="12" fill="#a78bfa" fontWeight={600}>
                    PnL {formatValue(hover.point.pnl)}
                  </text>
                </g>
              );
            })()}
          </g>
        )}
      </svg>

      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', color: 'var(--text-secondary)', marginTop: '0.25rem' }}>
        <span>{filtered.length} data points · Auto-refreshes every 10s</span>
        <span>
          Range: {formatValue(last?.equity ?? 0) ?? '-'}{' '}
          <span style={{ color: (last?.pnl ?? 0) >= 0 ? '#4ade80' : '#f87171' }}>
            {((last?.pnl ?? 0) >= 0 ? '+' : '')}{(last?.pnl ?? 0).toFixed(2)}
          </span>
        </span>
      </div>
    </div>
  );
}
