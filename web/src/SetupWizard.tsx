import React, { useState } from 'react';

export default function SetupWizard() {
  const [privateKey, setPrivateKey] = useState('');
  const [funderAddress, setFunderAddress] = useState('');
  const [targetWallets, setTargetWallets] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!privateKey.trim() || !funderAddress.trim()) {
      setError('Private Key and Funder Address are required.');
      return;
    }
    if (!targetWallets.trim()) {
      setError('At least one Target Wallet is required to start copy-trading.');
      return;
    }

    setLoading(true);
    setError(null);

    try {
      const res = await fetch('/api/setup', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          private_key: privateKey.trim(),
          funder_address: funderAddress.trim(),
          target_wallets: targetWallets.split(',').map((s: string) => s.trim()).filter((s: string) => s.length > 0),
        }),
      });

      if (!res.ok) {
        throw new Error('Failed to save configuration');
      }

      setSuccess(true);
      // Wait for the backend to hot-reboot, then poll until the API is ready
      const waitForDaemon = async () => {
        for (let i = 0; i < 30; i++) {
          await new Promise(r => setTimeout(r, 1000));
          try {
            const res = await fetch('/api/state');
            const data = await res.json();
            if (data.status !== 'setup_required') {
              window.location.reload();
              return;
            }
          } catch {
            // Daemon not ready yet, keep waiting
          }
        }
        // Fallback: reload after 30s anyway
        window.location.reload();
      };
      waitForDaemon();

    } catch (err: any) {
      setError(err.message || 'An error occurred during setup');
      setLoading(false);
    }
  };

  return (
    <div style={{ display: 'flex', minHeight: '100vh', alignItems: 'center', justifyContent: 'center', padding: '1rem' }}>
      <div className="glass-panel" style={{ width: '100%', maxWidth: '500px' }}>
        <div style={{ marginBottom: '1.5rem', textAlign: 'center', borderBottom: '1px solid var(--panel-border)', paddingBottom: '1rem' }}>
          <h2 style={{ fontSize: '1.5rem', fontWeight: 600, color: 'var(--text-primary)', margin: '0 0 0.5rem 0' }}>Polycopier Setup</h2>
          <div style={{ fontSize: '0.9rem', color: 'var(--text-secondary)', lineHeight: 1.4 }}>
            Welcome! Configure your credentials and target wallets to start copy-trading.
          </div>
        </div>

        {error && (
          <div style={{ padding: '0.75rem', background: 'rgba(239, 68, 68, 0.1)', color: 'var(--danger)', border: '1px solid rgba(239, 68, 68, 0.2)', borderRadius: '8px', marginBottom: '1.5rem', fontSize: '0.875rem' }}>
            {error}
          </div>
        )}

        {success && (
          <div style={{ padding: '0.75rem', background: 'rgba(16, 185, 129, 0.1)', color: 'var(--success)', border: '1px solid rgba(16, 185, 129, 0.2)', borderRadius: '8px', marginBottom: '1.5rem', fontSize: '0.875rem' }}>
            Setup successful! Initializing daemon...
          </div>
        )}

        <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: '1.5rem' }}>
          <div className="form-group">
            <label>Funder Address</label>
            <input 
              type="text" 
              value={funderAddress}
              onChange={(e) => setFunderAddress(e.target.value)}
              placeholder="0x..."
              style={{ width: '100%' }}
            />
            <span className="field-hint">Your Proxy Wallet address that holds your USDC</span>
          </div>

          <div className="form-group">
            <label>Private Key</label>
            <input 
              type="password" 
              value={privateKey}
              onChange={(e) => setPrivateKey(e.target.value)}
              placeholder="0x..."
              style={{ width: '100%' }}
            />
            <span className="field-hint">Your signing wallet private key (e.g. 0x...)</span>
          </div>

          <div className="form-group">
            <label>Target Wallets</label>
            <input 
              type="text" 
              value={targetWallets}
              onChange={(e) => setTargetWallets(e.target.value)}
              placeholder="0xWallet1, 0xWallet2..."
              style={{ width: '100%' }}
            />
            <span className="field-hint">Wallet addresses to copy-trade (comma separated, e.g. 0xabc..., 0xdef...)</span>
          </div>

          <button 
            type="submit" 
            className="action-button primary"
            style={{ width: '100%', marginTop: '1rem', justifyContent: 'center', padding: '0.75rem', fontSize: '1rem', fontWeight: 600 }}
            disabled={loading || success}
          >
            {loading ? 'Executing Hot Reboot...' : success ? 'Success' : 'Initialize Config'}
          </button>
        </form>
      </div>
    </div>
  );
}
