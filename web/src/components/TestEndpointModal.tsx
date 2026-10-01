import React, { useState } from 'react';
import { Check, Clock, Play, Send, X } from 'lucide-react';
import { api } from '../api/client';
import { ServiceItem } from '../types/amoeba';

interface TestEndpointModalProps {
  service: ServiceItem | null;
  onClose: () => void;
}

export const TestEndpointModal: React.FC<TestEndpointModalProps> = ({
  service,
  onClose,
}) => {
  const [method, setMethod] = useState<'GET' | 'POST' | 'PUT' | 'DELETE'>('GET');
  const [subpath, setSubpath] = useState('/');
  const [requestBody, setRequestBody] = useState('{\n  "query": "hello amoeba"\n}');
  const [isLoading, setIsLoading] = useState(false);
  const [response, setResponse] = useState<{
    status: number;
    data: any;
    durationMs: number;
  } | null>(null);

  if (!service) return null;

  const handleSend = async (e: React.FormEvent) => {
    e.preventDefault();
    setIsLoading(true);
    setResponse(null);

    const fullPath = `/v1/${service.name}${subpath.startsWith('/') ? subpath : `/${subpath}`}`;
    try {
      const res = await api.testEndpoint(
        fullPath,
        method,
        method !== 'GET' ? requestBody : undefined
      );
      setResponse(res);
    } catch (err: any) {
      setResponse({
        status: 500,
        data: { error: err.message || 'Request failed' },
        durationMs: 0,
      });
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4">
      <div className="w-full max-w-2xl rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200">
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-2.5">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
              <Play className="h-4 w-4 fill-emerald-400" />
            </div>
            <div>
              <h3 className="text-base font-semibold text-foreground font-mono">
                /v1/{service.name}/
              </h3>
              <p className="text-xs text-muted-foreground">Test scale-to-zero cold boots & API responses</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary/60 transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <form onSubmit={handleSend} className="my-4 space-y-4 text-xs">
          <div className="flex gap-2">
            <select
              value={method}
              onChange={(e: any) => setMethod(e.target.value)}
              className="rounded-md border border-input bg-background px-3 py-2 font-mono font-bold text-xs text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
            >
              <option value="GET">GET</option>
              <option value="POST">POST</option>
              <option value="PUT">PUT</option>
              <option value="DELETE">DELETE</option>
            </select>

            <div className="flex-1 relative flex items-center">
              <span className="absolute left-3 font-mono text-muted-foreground">/v1/{service.name}</span>
              <input
                type="text"
                value={subpath}
                onChange={(e) => setSubpath(e.target.value)}
                placeholder="/docs or /healthz"
                className="w-full rounded-md border border-input bg-background pl-28 sm:pl-36 pr-3 py-2 font-mono text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
              />
            </div>

            <button
              type="submit"
              disabled={isLoading}
              className="inline-flex items-center gap-1.5 rounded-md bg-emerald-600 hover:bg-emerald-500 px-4 py-2 font-semibold text-white transition-colors shadow-sm disabled:opacity-50"
            >
              <Send className="h-3.5 w-3.5" />
              <span>{isLoading ? 'Booting...' : 'Send'}</span>
            </button>
          </div>

          {method !== 'GET' && (
            <div>
              <label className="block font-medium text-foreground mb-1">Request Body (JSON)</label>
              <textarea
                value={requestBody}
                onChange={(e) => setRequestBody(e.target.value)}
                rows={4}
                className="w-full rounded-md border border-input bg-background p-2.5 font-mono text-[11px] text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
              />
            </div>
          )}
        </form>

        {response && (
          <div className="rounded-lg border border-border bg-muted/20 p-4 space-y-2 text-xs">
            <div className="flex items-center justify-between pb-2 border-b border-border/60">
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground font-medium">Status:</span>
                <span
                  className={`font-mono font-bold px-2 py-0.5 rounded text-xs ${
                    response.status >= 200 && response.status < 300
                      ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                      : 'bg-red-500/10 text-red-400 border border-red-500/20'
                  }`}
                >
                  {response.status}
                </span>
              </div>

              <div className="flex items-center gap-1 text-muted-foreground font-mono">
                <Clock className="h-3.5 w-3.5" />
                <span>{response.durationMs}ms</span>
                {response.durationMs > 200 && (
                  <span className="text-[10px] text-amber-400 ml-1">(Cold Boot Included)</span>
                )}
              </div>
            </div>

            <div>
              <span className="text-muted-foreground font-medium block mb-1">Response Body:</span>
              <pre className="p-3 rounded-md bg-background border border-border overflow-x-auto font-mono text-[11px] text-foreground max-h-56">
                {typeof response.data === 'object'
                  ? JSON.stringify(response.data, null, 2)
                  : String(response.data)}
              </pre>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
