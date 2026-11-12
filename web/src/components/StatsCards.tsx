import React from 'react';
import { Activity, Flame, Moon, Server } from 'lucide-react';
import { ServiceItem } from '../types/amoeba';

interface StatsCardsProps {
  services: ServiceItem[];
  isLoading: boolean;
}

export const StatsCards: React.FC<StatsCardsProps> = ({ services, isLoading }) => {
  const total = services.length;
  const warmCount = services.filter((s) => s.state.phase === 'Warm').length;
  const coldCount = services.filter((s) => s.state.phase === 'Cold').length;
  const activeRequests = services.reduce((acc, s) => acc + (s.state.active_requests || 0), 0);

  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 mb-8">
      <div className="rounded-xl border border-border bg-card p-4 shadow-sm relative overflow-hidden group">
        <div className="flex items-center justify-between">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Total Services</p>
          <div className="p-2 rounded-lg bg-blue-500/10 text-blue-400 border border-blue-500/20">
            <Server className="h-4 w-4" />
          </div>
        </div>
        <div className="mt-3 flex items-baseline gap-2">
          <p className="text-2xl font-bold tracking-tight text-foreground font-mono">
            {isLoading ? '...' : total}
          </p>
          <span className="text-xs text-muted-foreground">registered</span>
        </div>
      </div>

      <div className="rounded-xl border border-border bg-card p-4 shadow-sm relative overflow-hidden group">
        <div className="flex items-center justify-between">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Active Workloads</p>
          <div className="p-2 rounded-lg bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
            <Flame className="h-4 w-4" />
          </div>
        </div>
        <div className="mt-3 flex items-baseline gap-2">
          <p className="text-2xl font-bold tracking-tight text-emerald-400 font-mono">
            {isLoading ? '...' : warmCount}
          </p>
          <span className="text-xs text-muted-foreground">in-memory & warm</span>
        </div>
      </div>

      <div className="rounded-xl border border-border bg-card p-4 shadow-sm relative overflow-hidden group">
        <div className="flex items-center justify-between">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Scale-to-Zero</p>
          <div className="p-2 rounded-lg bg-indigo-500/10 text-indigo-400 border border-indigo-500/20">
            <Moon className="h-4 w-4" />
          </div>
        </div>
        <div className="mt-3 flex items-baseline gap-2">
          <p className="text-2xl font-bold tracking-tight text-indigo-400 font-mono">
            {isLoading ? '...' : coldCount}
          </p>
          <span className="text-xs text-muted-foreground">sleeping (0MB/0 CPU)</span>
        </div>
      </div>

      <div className="rounded-xl border border-border bg-card p-4 shadow-sm relative overflow-hidden group">
        <div className="flex items-center justify-between">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Gateway Inflight</p>
          <div className="p-2 rounded-lg bg-amber-500/10 text-amber-400 border border-amber-500/20">
            <Activity className="h-4 w-4" />
          </div>
        </div>
        <div className="mt-3 flex items-baseline gap-2">
          <p className="text-2xl font-bold tracking-tight text-amber-400 font-mono">
            {isLoading ? '...' : activeRequests}
          </p>
          <span className="text-xs text-muted-foreground">concurrent connections</span>
        </div>
      </div>
    </div>
  );
};
