import React, { useCallback, useEffect, useState } from 'react';
import { Check, Copy, Download, Key, Trash2, X } from 'lucide-react';
import { api } from '../api/client';
import { IssuedToken, ServiceTokenResponse } from '../types/amoeba';
import {
  TOKEN_LIFETIMES,
  copyText,
  downloadText,
  expiresIn,
  mcpClientConfig,
  serviceUrl,
} from '../lib/serviceToken';

interface ServiceTokenModalProps {
  serviceName: string | null;
  onClose: () => void;
}

const nowSecs = () => Math.floor(Date.now() / 1000);

export const ServiceTokenModal: React.FC<ServiceTokenModalProps> = ({
  serviceName,
  onClose,
}) => {
  const [label, setLabel] = useState('');
  const [org, setOrg] = useState('');
  const [ttlSeconds, setTtlSeconds] = useState(TOKEN_LIFETIMES[1].seconds);
  const [isLoading, setIsLoading] = useState(false);
  const [tokenResult, setTokenResult] = useState<ServiceTokenResponse | null>(null);
  const [issued, setIssued] = useState<IssuedToken[]>([]);
  const [copied, setCopied] = useState<'token' | 'config' | null>(null);
  const [mcpPath, setMcpPath] = useState('/mcp');
  const [error, setError] = useState<string | null>(null);

  const loadIssued = useCallback(async () => {
    if (!serviceName) return;
    try {
      setIssued(await api.listTokens(serviceName));
    } catch (err: any) {
      setError(err.message || 'Failed to list tokens');
    }
  }, [serviceName]);

  useEffect(() => {
    loadIssued();
  }, [loadIssued]);

  if (!serviceName) return null;

  const handleGenerate = async (e: React.FormEvent) => {
    e.preventDefault();
    setIsLoading(true);
    setError(null);
    try {
      const res = await api.issueToken(serviceName, {
        orgId: org.trim(),
        ttlSeconds,
        label: label.trim(),
      });
      setTokenResult(res);
      await loadIssued();
    } catch (err: any) {
      setError(err.message || 'Failed to issue token');
    } finally {
      setIsLoading(false);
    }
  };

  const handleRevoke = async (token: IssuedToken) => {
    setError(null);
    try {
      await api.revokeToken(serviceName, token.jti);
      if (tokenResult?.jti === token.jti) setTokenResult(null);
      await loadIssued();
    } catch (err: any) {
      setError(err.message || 'Failed to revoke token');
    }
  };

  const origin = window.location.origin;
  const configText = tokenResult
    ? JSON.stringify(mcpClientConfig(origin, serviceName, tokenResult.token, mcpPath), null, 2)
    : '';

  const handleCopy = async (what: 'token' | 'config') => {
    if (!tokenResult) return;
    const ok = await copyText(what === 'token' ? tokenResult.token : configText);
    if (ok) {
      setCopied(what);
      setTimeout(() => setCopied(null), 2000);
    } else {
      setError('Copy failed; select the text and copy it by hand, or download it.');
    }
  };

  const handleDownload = () => {
    if (!tokenResult) return;
    const suffix = tokenResult.label ? `-${tokenResult.label.replace(/[^A-Za-z0-9_-]+/g, '-')}` : '';
    downloadText(`${serviceName}${suffix}-mcp.json`, configText + '\n');
  };

  const inputClass =
    'w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono focus:outline-none focus:ring-1 focus:ring-ring';

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4 overflow-y-auto">
      <div className="w-full max-w-lg rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200 my-8">
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-2.5">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-blue-500/10 text-blue-400 border border-blue-500/20">
              <Key className="h-5 w-5" />
            </div>
            <div>
              <h3 className="text-base font-semibold text-foreground">Service Tokens</h3>
              <p className="text-xs text-muted-foreground font-mono">
                Service: {serviceName} · role mcp:{serviceName}
              </p>
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
          <div>
            <label className="block font-medium text-foreground mb-1">Label</label>
            <input
              type="text"
              value={label}
              maxLength={100}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="Cursor on laptop"
              className={inputClass}
            />
          </div>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
            <div>
              <label className="block font-medium text-foreground mb-1">Expires after</label>
              <select
                value={ttlSeconds}
                onChange={(e) => setTtlSeconds(Number(e.target.value))}
                className={inputClass}
              >
                {TOKEN_LIFETIMES.map((l) => (
                  <option key={l.seconds} value={l.seconds}>
                    {l.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="block font-medium text-foreground mb-1">Organization (optional)</label>
              <input
                type="text"
                value={org}
                onChange={(e) => setOrg(e.target.value)}
                placeholder="service's tenant"
                className={inputClass}
              />
            </div>
          </div>
          <p className="text-[11px] text-muted-foreground">
            The service's permissions must grant the role <code>mcp:{serviceName}</code>, unless it is public.
          </p>

          <button
            type="submit"
            disabled={isLoading}
            className="w-full inline-flex items-center justify-center gap-2 rounded-md bg-primary py-2 px-4 text-xs font-semibold text-primary-foreground hover:bg-primary/90 transition-colors shadow-sm disabled:opacity-50"
          >
            <Key className="h-3.5 w-3.5" />
            <span>{isLoading ? 'Issuing...' : 'Generate Token'}</span>
          </button>
        </form>

        {tokenResult && (
          <div className="rounded-lg border border-border bg-muted/20 p-4 space-y-3 text-xs">
            <p className="text-[11px] text-amber-400">
              This token is shown once. Amoeba keeps only its label and expiry, so copy or download it
              now and paste it into your AI app's MCP settings.
            </p>
            <div className="flex items-center justify-between">
              <span className="font-semibold text-foreground">
                Token · expires {expiresIn(tokenResult.expires_at, nowSecs())}
              </span>
              <button
                onClick={() => handleCopy('token')}
                className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80"
              >
                {copied === 'token' ? <Check className="h-3.5 w-3.5 text-emerald-400" /> : <Copy className="h-3.5 w-3.5" />}
                <span>{copied === 'token' ? 'Copied!' : 'Copy Token'}</span>
              </button>
            </div>

            <textarea
              readOnly
              value={tokenResult.token}
              rows={3}
              onFocus={(e) => e.target.select()}
              className="w-full rounded-md border border-border bg-background p-2 font-mono text-[11px] text-foreground focus:outline-none select-all"
            />

            <div className="space-y-2 pt-2 border-t border-border">
              <div className="flex items-center justify-between gap-2">
                <span className="font-semibold text-foreground">MCP client config</span>
                <div className="flex items-center gap-3">
                  <button
                    onClick={() => handleCopy('config')}
                    className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80"
                  >
                    {copied === 'config' ? <Check className="h-3.5 w-3.5 text-emerald-400" /> : <Copy className="h-3.5 w-3.5" />}
                    <span>{copied === 'config' ? 'Copied!' : 'Copy'}</span>
                  </button>
                  <button
                    onClick={handleDownload}
                    className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80"
                  >
                    <Download className="h-3.5 w-3.5" />
                    <span>Download</span>
                  </button>
                </div>
              </div>
              <label className="flex items-center gap-2 text-[11px] text-muted-foreground">
                <span className="shrink-0">MCP path in the app</span>
                <input
                  type="text"
                  value={mcpPath}
                  onChange={(e) => setMcpPath(e.target.value)}
                  className="w-full rounded-md border border-input bg-background px-2 py-1 text-foreground font-mono focus:outline-none focus:ring-1 focus:ring-ring"
                />
              </label>
              <pre className="p-2 rounded bg-background border border-border overflow-x-auto font-mono text-[10px] text-foreground">
                {configText}
              </pre>
            </div>

            <div className="text-[11px] text-muted-foreground">
              <span>Example curl command:</span>
              <pre className="mt-1 p-2 rounded bg-background border border-border overflow-x-auto font-mono text-[10px]">
                curl -H "Authorization: Bearer {tokenResult.token.slice(0, 16)}..." {serviceUrl(origin, serviceName)}
              </pre>
            </div>
          </div>
        )}

        <div className="mt-4 pt-4 border-t border-border text-xs">
          <span className="font-semibold text-foreground">Active tokens</span>
          {issued.length === 0 ? (
            <p className="mt-2 text-[11px] text-muted-foreground">No unexpired tokens for this service.</p>
          ) : (
            <ul className="mt-2 divide-y divide-border rounded-md border border-border">
              {issued.map((t) => (
                <li key={t.jti} className="flex items-center justify-between gap-3 px-3 py-2">
                  <div className="min-w-0">
                    <div className="truncate font-medium text-foreground">{t.label || 'Unlabeled'}</div>
                    <div className="truncate text-[11px] text-muted-foreground font-mono">
                      {t.org_id ? `${t.org_id} · ` : ''}expires {expiresIn(t.expires_at, nowSecs())} · {t.jti.slice(0, 8)}
                    </div>
                  </div>
                  <button
                    onClick={() => handleRevoke(t)}
                    className="inline-flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-[11px] font-medium text-red-400 hover:bg-red-500/10"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    <span>Revoke</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
};
