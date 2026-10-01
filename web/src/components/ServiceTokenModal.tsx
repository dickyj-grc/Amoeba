import React, { useState } from 'react';
import { Check, Copy, Key, X } from 'lucide-react';
import { api } from '../api/client';
import { ServiceTokenResponse } from '../types/amoeba';

interface ServiceTokenModalProps {
  serviceName: string | null;
  onClose: () => void;
}

export const ServiceTokenModal: React.FC<ServiceTokenModalProps> = ({
  serviceName,
  onClose,
}) => {
  const [role, setRole] = useState('read');
  const [org, setOrg] = useState('default');
  const [isLoading, setIsLoading] = useState(false);
  const [tokenResult, setTokenResult] = useState<ServiceTokenResponse | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!serviceName) return null;

  const handleGenerate = async (e: React.FormEvent) => {
    e.preventDefault();
    setIsLoading(true);
    setError(null);
    try {
      const res = await api.issueToken(serviceName, role, org);
      setTokenResult(res);
    } catch (err: any) {
      setError(err.message || 'Failed to issue token');
    } finally {
      setIsLoading(false);
    }
  };

  const handleCopy = () => {
    if (tokenResult?.token) {
      navigator.clipboard.writeText(tokenResult.token);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4">
      <div className="w-full max-w-lg rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200">
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-2.5">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-blue-500/10 text-blue-400 border border-blue-500/20">
              <Key className="h-5 w-5" />
            </div>
            <div>
              <h3 className="text-base font-semibold text-foreground">Issue Scoped JWT Token</h3>
              <p className="text-xs text-muted-foreground font-mono">Service: {serviceName}</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary/60 transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {error && (
          <div className="my-4 rounded-lg bg-destructive/10 border border-destructive/20 p-3 text-xs text-destructive-foreground">
            {error}
          </div>
        )}

        <form onSubmit={handleGenerate} className="my-4 space-y-4 text-xs">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="block font-medium text-foreground mb-1">Role</label>
              <select
                value={role}
                onChange={(e) => setRole(e.target.value)}
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono focus:outline-none focus:ring-1 focus:ring-ring"
              >
                <option value="read">read</option>
                <option value="add">add</option>
                <option value="update">update</option>
                <option value="delete">delete</option>
                <option value="admin">admin</option>
              </select>
            </div>

            <div>
              <label className="block font-medium text-foreground mb-1">Organization (Org)</label>
              <input
                type="text"
                value={org}
                onChange={(e) => setOrg(e.target.value)}
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono focus:outline-none focus:ring-1 focus:ring-ring"
                required
              />
            </div>
          </div>

          <button
            type="submit"
            disabled={isLoading}
            className="w-full inline-flex items-center justify-center gap-2 rounded-md bg-primary py-2 px-4 text-xs font-semibold text-primary-foreground hover:bg-primary/90 transition-colors shadow-sm disabled:opacity-50"
          >
            <Key className="h-3.5 w-3.5" />
            <span>{isLoading ? 'Issuing...' : 'Generate Scoped Token'}</span>
          </button>
        </form>

        {tokenResult && (
          <div className="rounded-lg border border-border bg-muted/20 p-4 space-y-3 text-xs">
            <div className="flex items-center justify-between">
              <span className="font-semibold text-foreground">Generated Token:</span>
              <button
                onClick={handleCopy}
                className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80"
              >
                {copied ? <Check className="h-3.5 w-3.5 text-emerald-400" /> : <Copy className="h-3.5 w-3.5" />}
                <span>{copied ? 'Copied!' : 'Copy Token'}</span>
              </button>
            </div>

            <textarea
              readOnly
              value={tokenResult.token}
              rows={3}
              className="w-full rounded-md border border-border bg-background p-2 font-mono text-[11px] text-foreground focus:outline-none select-all"
            />

            <div className="text-[11px] text-muted-foreground">
              <span>Example curl command:</span>
              <pre className="mt-1 p-2 rounded bg-background border border-border overflow-x-auto font-mono text-[10px]">
                curl -H "Authorization: Bearer {tokenResult.token.slice(0, 16)}..." http://localhost:8080/v1/{serviceName}/
              </pre>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
