import React, { useState } from 'react';
import { Key, Lock, Shield } from 'lucide-react';
import { api } from '../api/client';

interface LoginModalProps {
  isOpen: boolean;
  onSuccess: () => void;
}

export const LoginModal: React.FC<LoginModalProps> = ({ isOpen, onSuccess }) => {
  const [tab, setTab] = useState<'password' | 'token'>('token');
  const [username, setUsername] = useState('admin');
  const [password, setPassword] = useState('');
  const [tokenInput, setTokenInput] = useState('');
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!isOpen) return null;

  const handlePasswordLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setIsLoading(true);
    setError(null);
    try {
      await api.login(password, username);
      onSuccess();
    } catch (err: any) {
      setError(err.message || 'Login failed. Please check your credentials.');
    } finally {
      setIsLoading(false);
    }
  };

  const handleTokenSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!tokenInput.trim()) {
      setError('Please paste a valid Admin JWT token');
      return;
    }
    api.setToken(tokenInput.trim(), true);
    onSuccess();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4">
      <div className="w-full max-w-md rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200">
        <div className="flex items-center gap-3 mb-4">
          <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-blue-500/10 text-blue-400 border border-blue-500/20">
            <Shield className="h-5 w-5" />
          </div>
          <div>
            <h3 className="text-base font-semibold text-foreground">Authenticate to Amoeba</h3>
            <p className="text-xs text-muted-foreground">Access your zero-trust gateway control plane</p>
          </div>
        </div>

        {/* Tabs */}
        <div className="flex border-b border-border mb-4">
          <button
            onClick={() => { setTab('token'); setError(null); }}
            className={`flex-1 pb-2 text-xs font-medium border-b-2 transition-colors ${
              tab === 'token'
                ? 'border-primary text-primary font-semibold'
                : 'border-transparent text-muted-foreground hover:text-foreground'
            }`}
          >
            Admin JWT Token
          </button>
          <button
            onClick={() => { setTab('password'); setError(null); }}
            className={`flex-1 pb-2 text-xs font-medium border-b-2 transition-colors ${
              tab === 'password'
                ? 'border-primary text-primary font-semibold'
                : 'border-transparent text-muted-foreground hover:text-foreground'
            }`}
          >
            Username & Password
          </button>
        </div>

        {error && (
          <div className="mb-4 rounded-lg bg-destructive/10 border border-destructive/20 p-3 text-xs text-destructive-foreground">
            {error}
          </div>
        )}

        {tab === 'token' ? (
          <form onSubmit={handleTokenSubmit} className="space-y-4">
            <div>
              <label className="block text-xs font-medium text-foreground mb-1">
                Admin JWT Token
              </label>
              <textarea
                value={tokenInput}
                onChange={(e) => setTokenInput(e.target.value)}
                placeholder="eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."
                rows={4}
                className="w-full rounded-md border border-input bg-background p-2.5 text-xs font-mono text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
              <p className="mt-1 text-[11px] text-muted-foreground">
                Paste the token from <code className="bg-muted px-1 py-0.5 rounded">AMOEBA_TOKEN</code> or generated via CLI.
              </p>
            </div>

            <button
              type="submit"
              className="w-full inline-flex items-center justify-center gap-2 rounded-md bg-primary py-2 px-4 text-xs font-semibold text-primary-foreground hover:bg-primary/90 transition-colors shadow-sm"
            >
              <Key className="h-3.5 w-3.5" />
              <span>Connect to Amoeba</span>
            </button>
          </form>
        ) : (
          <form onSubmit={handlePasswordLogin} className="space-y-4">
            <div>
              <label className="block text-xs font-medium text-foreground mb-1">Username</label>
              <input
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-xs text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-foreground mb-1">Password</label>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-xs text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
            </div>

            <button
              type="submit"
              disabled={isLoading}
              className="w-full inline-flex items-center justify-center gap-2 rounded-md bg-primary py-2 px-4 text-xs font-semibold text-primary-foreground hover:bg-primary/90 transition-colors shadow-sm disabled:opacity-50"
            >
              <Lock className="h-3.5 w-3.5" />
              <span>{isLoading ? 'Signing in...' : 'Sign In'}</span>
            </button>
          </form>
        )}
      </div>
    </div>
  );
};
