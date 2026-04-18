import { useState } from 'react';

interface WalletStats {
  total_copies: number;
  wins: number;
  losses: number;
  total_pnl: string;
  consecutive_losses: number;
}

interface PnlSnapshot {
  timestamp_secs: number;
  realized_pnl: string;
  unrealized_pnl: string;
  total_balance: string;
}

interface PerfMetrics {
  started_at_secs: number;
  today_api_calls: number;
  last_api_latency_ms: number;
  avg_api_latency_ms: number;
  last_copy_latency_ms: number;
  avg_copy_latency_ms: number;
}

interface AiPanelProps {
  walletStats: Record<string, WalletStats>;
  mutedMarkets: string[];
  todayCopies: number;
  todayWins: number;
  todayLosses: number;
  todayPnl: string;
  isFrozen: boolean;
  freezeUntilSecs: number | null;
  walletBlacklist: string[];
  pnlHistory: PnlSnapshot[];
  perf: PerfMetrics;
  todayRealizedLoss: string;
  dailyStartBalance: string;
  dailyLossTriggered: boolean;
}

export default function AiPanel({
  walletStats,
  mutedMarkets,
  todayCopies,
  todayWins,
  todayLosses,
  todayPnl,
  isFrozen,
  freezeUntilSecs,
  walletBlacklist,
  pnlHistory,
  perf,
  todayRealizedLoss,
  dailyStartBalance,
  dailyLossTriggered,
}: AiPanelProps) {
  const [muteInput, setMuteInput] = useState('');
  const [muteMsg, setMuteMsg] = useState('');
  const [unmuteMsg, setUnmuteMsg] = useState('');
  const [tgToken, setTgToken] = useState('');
  const [tgChat, setTgChat] = useState('');
  const [tgMsg, setTgMsg] = useState('');

  const wallets = Object.entries(walletStats);
  const totalCopies: number = wallets.reduce((sum, [, s]) => sum + s.total_copies, 0);
  const totalWins: number = wallets.reduce((sum, [, s]) => sum + s.wins, 0);
  const overallWinRate = totalCopies > 0 ? (totalWins / totalCopies) * 100 : 0;

  const shortWallet = (w: string) =>
    w.length > 12 ? `${w.substring(0, 6)}...${w.substring(w.length - 4)}` : w;

  const formatPnl = (v: string) => {
    const n = parseFloat(v);
    if (isNaN(n)) return '$0.00';
    return `${n >= 0 ? '+' : ''}$${n.toFixed(2)}`;
  };

  const winRate = (wins: number, losses: number) => {
    const total = wins + losses;
    return total > 0 ? ((wins / total) * 100).toFixed(1) : '—';
  };

  const handleMute = async () => {
    const tokenId = muteInput.trim();
    if (!tokenId) return;
    try {
      const res = await fetch('/api/ai/markets/mute', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token_id: tokenId }),
      });
      const data = await res.json();
      setMuteMsg(data.muted ? 'Market muted' : 'Market unmuted');
      setUnmuteMsg('');
      setMuteInput('');
      setTimeout(() => setMuteMsg(''), 3000);
    } catch {
      setMuteMsg('Request failed');
    }
  };

  const handleUnmute = async (tokenId: string) => {
    try {
      const res = await fetch('/api/ai/markets/mute', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token_id: tokenId }),
      });
      const data = await res.json();
      setUnmuteMsg(data.muted ? 'Muted' : 'Unmuted');
      setMuteMsg('');
      setTimeout(() => setUnmuteMsg(''), 3000);
    } catch {
      setUnmuteMsg('Request failed');
    }
  };

  const handleTelegram = async () => {
    try {
      const res = await fetch('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ telegram: { bot_token: tgToken, chat_id: tgChat, min_pnl_usd: 0 } }),
      });
      const data = await res.json();
      setTgMsg(data.success ? 'Telegram config saved!' : 'Failed to save');
      setTimeout(() => setTgMsg(''), 3000);
    } catch {
      setTgMsg('Request failed');
    }
  };

  const freezeCountdown = freezeUntilSecs
    ? Math.max(0, Math.floor(freezeUntilSecs - Date.now() / 1000))
    : 0;

  const uptime = perf.started_at_secs
    ? Math.floor(Date.now() / 1000 - perf.started_at_secs)
    : 0;
  const uptimeStr = uptime > 3600
    ? `${Math.floor(uptime / 3600)}h ${Math.floor((uptime % 3600) / 60)}m`
    : `${Math.floor(uptime / 60)}m`;

  // Simple SVG equity curve
  const chartH = 120;
  const chartW = 500;
  const points = pnlHistory.slice(-120); // last 2h at 60s intervals
  const balances = points.map(p => parseFloat(p.total_balance));
  const minB = Math.min(...balances, 0);
  const maxB = Math.max(...balances, 1);
  const range = maxB - minB || 1;
  const pathD = points.map((p, i) => {
    const x = (i / Math.max(points.length - 1, 1)) * chartW;
    const y = chartH - ((parseFloat(p.total_balance) - minB) / range) * (chartH - 10) - 5;
    return `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(' ');

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '1.5rem' }}>

      {/* Freeze Alert Banner */}
      {isFrozen && (
        <div className="glass-panel" style={{
          background: 'rgba(239,68,68,0.12)',
          border: '1px solid rgba(239,68,68,0.4)',
          padding: '12px 16px',
        }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px', color: '#f87171', fontWeight: 600 }}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/>
            </svg>
            AI Freeze Active
            {freezeCountdown > 0 && (
              <span style={{ marginLeft: '8px', fontFamily: 'monospace', fontSize: '0.85rem' }}>
                {Math.floor(freezeCountdown / 60)}m {freezeCountdown % 60}s remaining
              </span>
            )}
          </div>
        </div>
      )}

      {/* Daily Loss Circuit-Breaker Banner */}
      {dailyLossTriggered && (
        <div className="glass-panel" style={{
          background: 'rgba(220,38,38,0.15)',
          border: '1px solid rgba(220,38,38,0.5)',
          padding: '12px 16px',
        }}>
          <div style={{ color: '#fca5a5', fontWeight: 600 }}>
            Daily Loss Circuit-Breaker Triggered
            <span style={{ marginLeft: '8px', fontWeight: 400, fontSize: '0.85rem' }}>
              Loss: ${parseFloat(todayRealizedLoss).toFixed(2)} / Starting: ${parseFloat(dailyStartBalance).toFixed(2)}
            </span>
          </div>
        </div>
      )}

      {/* Today's Stats */}
      <div className="glass-panel">
        <div className="panel-header">
          Today&apos;s Stats
          <span className="panel-subtitle">UTC {new Date().toUTCString().slice(0, 16)}</span>
        </div>
        <div className="stats-grid" style={{ marginTop: '0.75rem' }}>
          <div className="stat-card">
            <span className="stat-label">Copies Today</span>
            <span className="stat-value">{todayCopies}</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Wins</span>
            <span className="stat-value val-positive">{todayWins}</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Losses</span>
            <span className="stat-value val-negative">{todayLosses}</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Win Rate</span>
            <span className={`stat-value ${todayCopies > 0 ? (todayWins / todayCopies >= 0.5 ? 'val-positive' : 'val-negative') : ''}`}>
              {todayCopies > 0 ? `${(todayWins / todayCopies * 100).toFixed(1)}%` : '—'}
            </span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Today&apos;s PnL</span>
            <span className={`stat-value ${parseFloat(todayPnl) >= 0 ? 'val-positive' : 'val-negative'}`}>
              {formatPnl(todayPnl)}
            </span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Today&apos;s Loss</span>
            <span className="stat-value val-negative">${parseFloat(todayRealizedLoss).toFixed(2)}</span>
          </div>
        </div>
      </div>

      {/* Equity Curve */}
      <div className="glass-panel">
        <div className="panel-header">
          Equity Curve
          <span className="panel-subtitle">{points.length} data points</span>
        </div>
        <div style={{ padding: '12px', overflowX: 'auto' }}>
          {points.length > 2 ? (
            <svg width={chartW} height={chartH} viewBox={`0 0 ${chartW} ${chartH}`} style={{ width: '100%', maxWidth: chartW }}>
              <line x1="0" y1={chartH - 5} x2={chartW} y2={chartH - 5} stroke="rgba(255,255,255,0.1)" />
              <text x="4" y="12" fill="rgba(255,255,255,0.4)" fontSize="9">${maxB.toFixed(0)}</text>
              <text x="4" y={chartH - 8} fill="rgba(255,255,255,0.4)" fontSize="9">${minB.toFixed(0)}</text>
              <path d={pathD} fill="none" stroke="#60a5fa" strokeWidth="1.5" />
            </svg>
          ) : (
            <div style={{ color: 'var(--text-secondary)', textAlign: 'center', padding: '2rem' }}>
              Equity curve will appear after a few minutes of runtime...
            </div>
          )}
        </div>
      </div>

      {/* Performance Monitor */}
      <div className="glass-panel">
        <div className="panel-header">
          Performance Monitor
          <span className="panel-subtitle">Uptime: {uptimeStr}</span>
        </div>
        <div className="stats-grid" style={{ marginTop: '0.75rem' }}>
          <div className="stat-card">
            <span className="stat-label">API Latency</span>
            <span className="stat-value">{perf.last_api_latency_ms}ms</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Avg API Latency</span>
            <span className="stat-value">{perf.avg_api_latency_ms}ms</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Copy Latency</span>
            <span className="stat-value">{perf.last_copy_latency_ms}ms</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">Avg Copy Latency</span>
            <span className="stat-value">{perf.avg_copy_latency_ms}ms</span>
          </div>
          <div className="stat-card">
            <span className="stat-label">API Calls Today</span>
            <span className="stat-value">{perf.today_api_calls}</span>
          </div>
        </div>
      </div>

      {/* Per-Wallet Stats + Blacklist */}
      <div className="glass-panel">
        <div className="panel-header">
          Per-Wallet Statistics
          <span className="panel-subtitle">
            {wallets.length} wallets · {totalCopies} total copies · {overallWinRate.toFixed(1)}% overall win rate
          </span>
        </div>
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Wallet</th>
                <th>Total Copies</th>
                <th>Wins</th>
                <th>Losses</th>
                <th>Consec Loss</th>
                <th>Win Rate</th>
                <th>Cumulative PnL</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {wallets.map(([wallet, stats]) => {
                const pnl = parseFloat(stats.total_pnl);
                const isBL = walletBlacklist.includes(wallet);
                return (
                  <tr key={wallet} style={isBL ? { opacity: 0.5 } : {}}>
                    <td>
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                        <span style={{ fontFamily: 'monospace', fontSize: '0.8rem' }} title={wallet}>
                          {shortWallet(wallet)}
                        </span>
                        {isBL && (
                          <span style={{ fontSize: '0.65rem', color: '#f87171', background: 'rgba(239,68,68,0.15)', padding: '1px 4px', borderRadius: '3px' }}>
                            BL
                          </span>
                        )}
                      </div>
                    </td>
                    <td>{stats.total_copies}</td>
                    <td className="val-positive">{stats.wins}</td>
                    <td className="val-negative">{stats.losses}</td>
                    <td className={stats.consecutive_losses >= 3 ? 'val-negative' : ''}>{stats.consecutive_losses}</td>
                    <td className={parseFloat(winRate(stats.wins, stats.losses)) >= 50 ? 'val-positive' : ''}>
                      {winRate(stats.wins, stats.losses)}%
                    </td>
                    <td className={pnl >= 0 ? 'val-positive' : 'val-negative'}>
                      {formatPnl(stats.total_pnl)}
                    </td>
                    <td>
                      <button
                        onClick={async () => {
                          await fetch('/api/wallet/blacklist', {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({ wallet }),
                          });
                        }}
                        style={{
                          padding: '2px 8px',
                          fontSize: '0.65rem',
                          background: isBL ? 'rgba(74,222,128,0.15)' : 'rgba(239,68,68,0.15)',
                          border: `1px solid ${isBL ? 'rgba(74,222,128,0.4)' : 'rgba(239,68,68,0.4)'}`,
                          borderRadius: '3px',
                          color: isBL ? '#4ade80' : '#f87171',
                          cursor: 'pointer',
                        }}
                      >
                        {isBL ? 'Unblock' : 'Block'}
                      </button>
                    </td>
                  </tr>
                );
              })}
              {wallets.length === 0 && (
                <tr>
                  <td colSpan={8} style={{ textAlign: 'center', color: 'var(--text-secondary)' }}>
                    No copy history yet. Stats will appear after positions are closed.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* Market Mute */}
      <div className="glass-panel">
        <div className="panel-header">
          Market Mute
          <span className="panel-subtitle">
            Muted: {mutedMarkets.length} market{mutedMarkets.length !== 1 ? 's' : ''}
          </span>
        </div>
        <div style={{ padding: '0.75rem', display: 'flex', flexDirection: 'column', gap: '0.75rem' }}>
          <div style={{ display: 'flex', gap: '8px' }}>
            <input
              type="text"
              placeholder="Token ID to mute"
              value={muteInput}
              onChange={e => setMuteInput(e.target.value)}
              onKeyDown={e => e.key === 'Enter' && handleMute()}
              style={{
                flex: 1,
                background: 'rgba(255,255,255,0.06)',
                border: '1px solid rgba(255,255,255,0.15)',
                borderRadius: '6px',
                padding: '8px 12px',
                color: 'var(--text-primary)',
                fontSize: '0.875rem',
                outline: 'none',
              }}
            />
            <button
              onClick={handleMute}
              disabled={!muteInput.trim()}
              style={{
                padding: '8px 16px',
                background: 'rgba(239,68,68,0.2)',
                border: '1px solid rgba(239,68,68,0.4)',
                borderRadius: '6px',
                color: muteInput.trim() ? '#f87171' : 'var(--text-secondary)',
                cursor: muteInput.trim() ? 'pointer' : 'not-allowed',
                fontSize: '0.875rem',
                fontWeight: 500,
              }}
            >
              Mute
            </button>
          </div>
          {muteMsg && <div style={{ color: '#f87171', fontSize: '0.8rem' }}>{muteMsg}</div>}
          {mutedMarkets.length > 0 && (
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: '8px' }}>
              {mutedMarkets.map(m => (
                <div key={m} style={{
                  display: 'flex', alignItems: 'center', gap: '6px',
                  background: 'rgba(239,68,68,0.1)', border: '1px solid rgba(239,68,68,0.3)',
                  borderRadius: '4px', padding: '4px 8px', fontSize: '0.75rem', fontFamily: 'monospace', color: '#fca5a5',
                }}>
                  <span title={m}>{m.substring(0, 12)}...</span>
                  <button onClick={() => handleUnmute(m)} style={{ background: 'none', border: 'none', cursor: 'pointer', color: '#f87171', padding: '0', fontSize: '1rem', lineHeight: 1 }}>×</button>
                </div>
              ))}
            </div>
          )}
          {unmuteMsg && <div style={{ color: '#4ade80', fontSize: '0.8rem' }}>{unmuteMsg}</div>}
        </div>
      </div>

      {/* AI Controls */}
      <div className="glass-panel">
        <div className="panel-header">AI Controls</div>
        <div style={{ padding: '0.75rem', display: 'flex', flexWrap: 'wrap', gap: '12px' }}>
          <AiActionButton label="Freeze 1h" color="#f87171" colorRgb="239,68,68" action="3600" />
          <AiActionButton label="Freeze 4h" color="#fb923c" colorRgb="251,146,60" action="14400" />
          <AiActionButton label="Freeze 24h" color="#f97316" colorRgb="249,115,22" action="86400" />
          <AiActionButton label="Emergency Close All" color="#dc2626" colorRgb="220,38,38" action="close_all" />
          {isFrozen && (
            <AiActionButton label="Unfreeze Now" color="#4ade80" colorRgb="74,222,128" action="unfreeze" />
          )}
          <a href="/api/csv/export" download style={{ textDecoration: 'none' }}>
            <AiActionButton label="Export CSV" color="#60a5fa" colorRgb="96,165,250" action="csv" />
          </a>
        </div>
      </div>

      {/* Telegram Config */}
      <div className="glass-panel">
        <div className="panel-header">
          Telegram Notifications
          <span className="panel-subtitle">Optional — configure to receive trade alerts</span>
        </div>
        <div style={{ padding: '0.75rem', display: 'flex', flexDirection: 'column', gap: '8px' }}>
          <div style={{ display: 'flex', gap: '8px' }}>
            <input
              type="text"
              placeholder="Bot Token (from @BotFather)"
              value={tgToken}
              onChange={e => setTgToken(e.target.value)}
              style={{
                flex: 1,
                background: 'rgba(255,255,255,0.06)',
                border: '1px solid rgba(255,255,255,0.15)',
                borderRadius: '6px',
                padding: '8px 12px',
                color: 'var(--text-primary)',
                fontSize: '0.8rem',
                outline: 'none',
              }}
            />
            <input
              type="text"
              placeholder="Chat ID"
              value={tgChat}
              onChange={e => setTgChat(e.target.value)}
              style={{
                width: '140px',
                background: 'rgba(255,255,255,0.06)',
                border: '1px solid rgba(255,255,255,0.15)',
                borderRadius: '6px',
                padding: '8px 12px',
                color: 'var(--text-primary)',
                fontSize: '0.8rem',
                outline: 'none',
              }}
            />
            <button
              onClick={handleTelegram}
              disabled={!tgToken.trim() || !tgChat.trim()}
              style={{
                padding: '8px 16px',
                background: 'rgba(96,165,250,0.2)',
                border: '1px solid rgba(96,165,250,0.4)',
                borderRadius: '6px',
                color: tgToken.trim() && tgChat.trim() ? '#60a5fa' : 'var(--text-secondary)',
                cursor: tgToken.trim() && tgChat.trim() ? 'pointer' : 'not-allowed',
                fontSize: '0.8rem',
                fontWeight: 500,
              }}
            >
              Save
            </button>
          </div>
          {tgMsg && <div style={{ color: '#60a5fa', fontSize: '0.8rem' }}>{tgMsg}</div>}
        </div>
      </div>
    </div>
  );
}

function AiActionButton({
  label, color, colorRgb, action
}: {
  label: string;
  color: string;
  colorRgb: string;
  action: string;
}) {
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState('');

  const isUnfreeze = action === 'unfreeze';
  const isCloseAll = action === 'close_all';
  const isCsv = action === 'csv';
  const endpoint = isUnfreeze ? '/ai/unfreeze' : isCloseAll ? '/ai/close' : isCsv ? '' : '/ai/freeze';
  const body = isCloseAll ? '{}' : isUnfreeze ? '{}' : JSON.stringify({ duration_secs: parseInt(action) });

  const handle = async () => {
    if (isCsv) return; // handled by <a href>
    setLoading(true);
    setResult('');
    try {
      const res = await fetch(endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body,
      });
      const data = await res.json();
      setResult(data.message || data.error || 'Done');
    } catch {
      setResult('Request failed');
    }
    setLoading(false);
    setTimeout(() => setResult(''), 5000);
  };

  return (
    <div style={{
      flex: '1 1 140px',
      background: `rgba(${colorRgb},0.08)`,
      border: `1px solid rgba(${colorRgb},0.3)`,
      borderRadius: '8px',
      padding: '12px',
      display: 'flex',
      flexDirection: 'column',
      gap: '4px',
    }}>
      <button
        onClick={handle}
        disabled={loading}
        style={{
          background: `rgba(${colorRgb},0.15)`,
          border: `1px solid rgba(${colorRgb},0.4)`,
          borderRadius: '4px',
          color,
          cursor: loading ? 'wait' : 'pointer',
          padding: '6px 12px',
          fontSize: '0.8rem',
          fontWeight: 600,
          textAlign: 'left',
        }}
      >
        {loading ? '...' : label}
      </button>
      {result && <div style={{ fontSize: '0.7rem', color, marginTop: '2px' }}>{result}</div>}
    </div>
  );
}
