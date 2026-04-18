import { useState } from 'react';

interface WalletStats {
  total_copies: number;
  wins: number;
  losses: number;
  total_pnl: string;
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
}: AiPanelProps) {
  const [muteInput, setMuteInput] = useState('');
  const [muteMsg, setMuteMsg] = useState('');
  const [unmuteMsg, setUnmuteMsg] = useState('');

  const wallets = Object.entries(walletStats);
  const totalCopies: number = wallets.reduce((sum, [, s]) => sum + s.total_copies, 0);
  const totalWins: number = wallets.reduce((sum, [, s]) => sum + s.wins, 0);
  const totalLosses: number = wallets.reduce((sum, [, s]) => sum + s.losses, 0);
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
      const res = await fetch('/api/market/mute', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token_id: tokenId }),
      });
      const data = await res.json();
      setMuteMsg(data.message || 'Muted successfully');
      setUnmuteMsg('');
      setMuteInput('');
      setTimeout(() => setMuteMsg(''), 3000);
    } catch {
      setMuteMsg('Request failed');
    }
  };

  const handleUnmute = async (tokenId: string) => {
    try {
      const res = await fetch('/api/market/unmute', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ token_id: tokenId }),
      });
      const data = await res.json();
      setUnmuteMsg(data.message || 'Unmuted successfully');
      setMuteMsg('');
      setTimeout(() => setUnmuteMsg(''), 3000);
    } catch {
      setUnmuteMsg('Request failed');
    }
  };

  const freezeCountdown = freezeUntilSecs
    ? Math.max(0, Math.floor(freezeUntilSecs - Date.now() / 1000))
    : 0;

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
          <div style={{ color: '#fca5a5', fontSize: '0.8rem', marginTop: '4px' }}>
            All copy-trading is paused. Use <code style={{ background: 'rgba(255,255,255,0.1)', padding: '1px 4px', borderRadius: '3px' }}>POST /ai/unfreeze</code> to resume early.
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
        </div>
      </div>

      {/* Per-Wallet Stats */}
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
                <th>Win Rate</th>
                <th>Cumulative PnL</th>
              </tr>
            </thead>
            <tbody>
              {wallets.map(([wallet, stats]) => {
                const pnl = parseFloat(stats.total_pnl);
                return (
                  <tr key={wallet}>
                    <td>
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                        <span style={{ fontFamily: 'monospace', fontSize: '0.8rem' }} title={wallet}>
                          {shortWallet(wallet)}
                        </span>
                        <button
                          onClick={() => navigator.clipboard.writeText(wallet)}
                          style={{ background: 'none', border: 'none', cursor: 'pointer', padding: '2px', color: 'var(--text-secondary)', display: 'flex' }}
                          title="Copy address"
                        >
                          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                            <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
                            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
                          </svg>
                        </button>
                      </div>
                    </td>
                    <td>{stats.total_copies}</td>
                    <td className="val-positive">{stats.wins}</td>
                    <td className="val-negative">{stats.losses}</td>
                    <td className={parseFloat(winRate(stats.wins, stats.losses)) >= 50 ? 'val-positive' : ''}>
                      {winRate(stats.wins, stats.losses)}%
                    </td>
                    <td className={pnl >= 0 ? 'val-positive' : 'val-negative'}>
                      {formatPnl(stats.total_pnl)}
                    </td>
                  </tr>
                );
              })}
              {wallets.length === 0 && (
                <tr>
                  <td colSpan={6} style={{ textAlign: 'center', color: 'var(--text-secondary)' }}>
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
            Muted: {mutedMarkets.length} market{mutedMarkets.length !== 1 ? 's' : ''} — copy-trading is skipped for muted markets
          </span>
        </div>
        <div style={{ padding: '0.75rem', display: 'flex', flexDirection: 'column', gap: '0.75rem' }}>
          <div style={{ display: 'flex', gap: '8px' }}>
            <input
              type="text"
              placeholder="Token ID to mute (e.g. 0x1234...)"
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
                background: muteInput.trim() ? 'rgba(239,68,68,0.2)' : 'rgba(255,255,255,0.05)',
                border: '1px solid rgba(239,68,68,0.4)',
                borderRadius: '6px',
                color: muteInput.trim() ? '#f87171' : 'var(--text-secondary)',
                cursor: muteInput.trim() ? 'pointer' : 'not-allowed',
                fontSize: '0.875rem',
                fontWeight: 500,
              }}
            >
              Mute Market
            </button>
          </div>
          {muteMsg && (
            <div style={{ color: '#f87171', fontSize: '0.8rem' }}>{muteMsg}</div>
          )}

          {mutedMarkets.length > 0 && (
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: '8px', marginTop: '4px' }}>
              {mutedMarkets.map(m => (
                <div
                  key={m}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: '6px',
                    background: 'rgba(239,68,68,0.1)',
                    border: '1px solid rgba(239,68,68,0.3)',
                    borderRadius: '4px',
                    padding: '4px 8px',
                    fontSize: '0.75rem',
                    fontFamily: 'monospace',
                    color: '#fca5a5',
                  }}
                >
                  <span title={m}>{m.substring(0, 12)}...</span>
                  <button
                    onClick={() => handleUnmute(m)}
                    style={{
                      background: 'none',
                      border: 'none',
                      cursor: 'pointer',
                      color: '#f87171',
                      padding: '0',
                      display: 'flex',
                      fontSize: '1rem',
                      lineHeight: 1,
                    }}
                    title="Unmute"
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
          {unmuteMsg && (
            <div style={{ color: '#4ade80', fontSize: '0.8rem' }}>{unmuteMsg}</div>
          )}
        </div>
      </div>

      {/* AI Actions */}
      <div className="glass-panel">
        <div className="panel-header">AI Controls</div>
        <div style={{ padding: '0.75rem', display: 'flex', flexWrap: 'wrap', gap: '12px' }}>
          <AiActionButton
            label="Freeze 1h"
            sublabel="Pause all copy-trading for 1 hour"
            color="#f87171"
            colorRgb="239,68,68"
            action="3600"
          />
          <AiActionButton
            label="Freeze 4h"
            sublabel="Pause all copy-trading for 4 hours"
            color="#fb923c"
            colorRgb="251,146,60"
            action="14400"
          />
          <AiActionButton
            label="Freeze 24h"
            sublabel="Pause all copy-trading for 24 hours"
            color="#f97316"
            colorRgb="249,115,22"
            action="86400"
          />
          {!isFrozen && (
            <AiActionButton
              label="Emergency Close All"
              sublabel="Close all positions immediately"
              color="#dc2626"
              colorRgb="220,38,38"
              action="close_all"
            />
          )}
          {isFrozen && (
            <AiActionButton
              label="Unfreeze Now"
              sublabel="Resume copy-trading immediately"
              color="#4ade80"
              colorRgb="74,222,128"
              action="unfreeze"
            />
          )}
        </div>
      </div>
    </div>
  );
}

function AiActionButton({
  label, sublabel, color, colorRgb, action
}: {
  label: string;
  sublabel: string;
  color: string;
  colorRgb: string;
  action: string;
}) {
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState('');

  const isUnfreeze = action === 'unfreeze';
  const isCloseAll = action === 'close_all';
  const endpoint = isUnfreeze ? '/ai/unfreeze' : isCloseAll ? '/ai/close' : '/ai/freeze';
  const method = isCloseAll ? 'POST' : 'POST';
  const body = isCloseAll ? '{}' : isUnfreeze ? '{}' : JSON.stringify({ duration: parseInt(action) });

  const handle = async () => {
    setLoading(true);
    setResult('');
    try {
      const res = await fetch(endpoint, {
        method,
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
    <div
      style={{
        flex: '1 1 180px',
        background: `rgba(${colorRgb},0.08)`,
        border: `1px solid rgba(${colorRgb},0.3)`,
        borderRadius: '8px',
        padding: '12px',
        display: 'flex',
        flexDirection: 'column',
        gap: '4px',
      }}
    >
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
      <div style={{ fontSize: '0.7rem', color: 'var(--text-secondary)' }}>{sublabel}</div>
      {result && (
        <div style={{ fontSize: '0.7rem', color, marginTop: '2px' }}>{result}</div>
      )}
    </div>
  );
}
