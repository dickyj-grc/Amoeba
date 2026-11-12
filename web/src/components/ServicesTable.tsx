import React, { useState } from 'react';
import {
  AlertTriangle,
  ExternalLink,
  Key,
  Moon,
  Play,
  RefreshCw,
  Search,
  Trash2,
} from 'lucide-react';
import { ServiceItem } from '../types/amoeba';
import { interfaceLabel } from '../lib/library';

interface ServicesTableProps {
  services: ServiceItem[];
  isLoading: boolean;
  onRefresh: () => void;
  onUninstall: (name: string) => Promise<void>;
  onTestEndpoint: (service: ServiceItem) => void;
  onIssueToken: (serviceName: string) => void;
}

export const ServicesTable: React.FC<ServicesTableProps> = ({
  services,
  isLoading,
  onRefresh,
  onUninstall,
  onTestEndpoint,
  onIssueToken,
}) => {
  const [searchTerm, setSearchTerm] = useState('');
  const [deletingName, setDeletingName] = useState<string | null>(null);

  const filteredServices = services.filter((s) =>
    s.name.toLowerCase().includes(searchTerm.toLowerCase())
  );

  const handleDelete = async (name: string) => {
    if (!window.confirm(`Are you sure you want to uninstall and remove "${name}" from Amoeba?`)) {
      return;
    }
    setDeletingName(name);
    try {
      await onUninstall(name);
    } finally {
      setDeletingName(null);
    }
  };

  const renderStatusBadge = (phase: string) => {
    switch (phase) {
      case 'Warm':
        return (
          <span className="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/10 px-2.5 py-1 text-xs font-medium text-emerald-400 border border-emerald-500/20">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
            Warm (Running)
          </span>
        );
      case 'Warming':
        return (
          <span className="inline-flex items-center gap-1.5 rounded-full bg-amber-500/10 px-2.5 py-1 text-xs font-medium text-amber-400 border border-amber-500/20">
            <RefreshCw className="h-3 w-3 animate-spin" />
            Preparing
          </span>
        );
      case 'Error':
        return (
          <span className="inline-flex items-center gap-1.5 rounded-full bg-red-500/10 px-2.5 py-1 text-xs font-medium text-red-400 border border-red-500/20">
            <AlertTriangle className="h-3 w-3" />
            Error
          </span>
        );
      case 'Cold':
      case 'Installed':
      default:
        return (
          <span className="inline-flex items-center gap-1.5 rounded-full bg-slate-500/10 px-2.5 py-1 text-xs font-medium text-slate-400 border border-slate-500/20">
            <Moon className="h-3 w-3 text-slate-400" />
            Idle (Scale-to-Zero)
          </span>
        );
    }
  };

  return (
    <div className="rounded-xl border border-border bg-card shadow-sm overflow-hidden">
      <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-3 p-4 border-b border-border bg-muted/20">
        <div>
          <h2 className="text-base font-semibold text-foreground">Installed Microservices</h2>
          <p className="text-xs text-muted-foreground">
            Manage scale-to-zero workloads, routing gates, and credentials
          </p>
        </div>

        <div className="flex items-center gap-2">
          <div className="relative">
            <Search className="absolute left-2.5 top-2.5 h-3.5 w-3.5 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search services..."
              value={searchTerm}
              onChange={(e) => setSearchTerm(e.target.value)}
              className="h-8.5 w-48 sm:w-64 rounded-md border border-input bg-background pl-8 pr-3 text-xs text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
            />
          </div>

          <button
            onClick={onRefresh}
            disabled={isLoading}
            className="inline-flex items-center justify-center h-8.5 w-8.5 rounded-md border border-input bg-background hover:bg-secondary/80 text-muted-foreground hover:text-foreground transition-colors"
            title="Refresh services"
          >
            <RefreshCw className={`h-3.5 w-3.5 ${isLoading ? 'animate-spin text-primary' : ''}`} />
          </button>
        </div>
      </div>

      <div className="overflow-x-auto">
        <table className="w-full text-left text-xs">
          <thead className="bg-muted/40 text-muted-foreground font-medium border-b border-border">
            <tr>
              <th className="py-3 px-4">Service Name</th>
              <th className="py-3 px-4">Status & Lifecycle</th>
              <th className="py-3 px-4">Proxy Gateway Route</th>
              <th className="py-3 px-4 text-center">Active Requests</th>
              <th className="py-3 px-4 text-right">Actions</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border/60">
            {filteredServices.length === 0 ? (
              <tr>
                <td colSpan={5} className="py-12 text-center text-muted-foreground">
                  {isLoading ? (
                    <div className="flex items-center justify-center gap-2">
                      <RefreshCw className="h-4 w-4 animate-spin text-primary" />
                      <span>Loading services from Amoeba catalog...</span>
                    </div>
                  ) : searchTerm ? (
                    <span>No services matching "{searchTerm}"</span>
                  ) : (
                    <div className="space-y-2">
                      <p className="font-medium text-foreground">No services deployed yet</p>
                      <p className="text-xs">Deploy an API from GitHub, a Dockerfile, or an image to get started.</p>
                    </div>
                  )}
                </td>
              </tr>
            ) : (
              filteredServices.map((service) => (
                <tr key={service.name} className="hover:bg-muted/30 transition-colors group">
                  <td className="py-3.5 px-4">
                    <div className="flex items-center gap-2">
                      <span className="font-semibold text-foreground font-mono text-sm">
                        {service.name}
                      </span>
                    </div>
                    {service.port !== undefined && (
                      <span className="text-[11px] text-muted-foreground font-mono">port {service.port}</span>
                    )}
                  </td>
                  <td className="py-3.5 px-4">
                    {renderStatusBadge(service.state.phase)}
                    {service.state.error && (
                      <p className="mt-1 max-w-xs text-[11px] leading-snug text-red-400/90 break-words">
                        {service.state.error}
                      </p>
                    )}
                  </td>
                  <td className="py-3.5 px-4">
                    <a
                      href={`/v1/${service.name}/`}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 font-mono text-blue-400 hover:text-blue-300 hover:underline"
                    >
                      /v1/{service.name}/
                      <ExternalLink className="h-3 w-3 opacity-60" />
                    </a>
                    {service.interface && (
                      <p
                        className="mt-1 max-w-xs text-[11px] leading-snug text-muted-foreground"
                        title={service.interface.summary}
                      >
                        <span className="font-medium text-foreground">
                          {interfaceLabel(service.interface.type)}
                          {service.interface.path ? ` at ${service.interface.path}` : ''}
                        </span>
                        {service.interface.summary ? ` · ${service.interface.summary}` : ''}
                        {service.interface.docs && (
                          <>
                            {' '}
                            <a
                              href={service.interface.docs}
                              target="_blank"
                              rel="noopener noreferrer"
                              className="text-blue-400 hover:underline"
                            >
                              Docs
                            </a>
                          </>
                        )}
                      </p>
                    )}
                  </td>
                  <td className="py-3.5 px-4 text-center">
                    <span
                      className={`font-mono font-medium ${
                        service.state.active_requests > 0 ? 'text-amber-400' : 'text-muted-foreground'
                      }`}
                    >
                      {service.state.active_requests || 0}
                    </span>
                  </td>
                  <td className="py-3.5 px-4 text-right">
                    <div className="flex items-center justify-end gap-1.5">
                      <button
                        onClick={() => onTestEndpoint(service)}
                        className="inline-flex items-center gap-1 px-2.5 py-1 rounded bg-secondary hover:bg-secondary/80 text-foreground text-xs font-medium border border-border transition-colors"
                        title="Test API Endpoint"
                      >
                        <Play className="h-3 w-3 text-emerald-400 fill-emerald-400" />
                        <span>Test</span>
                      </button>

                      <button
                        onClick={() => onIssueToken(service.name)}
                        className="inline-flex items-center gap-1 px-2.5 py-1 rounded bg-secondary hover:bg-secondary/80 text-muted-foreground hover:text-foreground text-xs font-medium border border-border transition-colors"
                        title="Issue Scoped JWT Token"
                      >
                        <Key className="h-3 w-3 text-blue-400" />
                        <span>Token</span>
                      </button>

                      <button
                        onClick={() => handleDelete(service.name)}
                        disabled={deletingName === service.name}
                        className="inline-flex items-center gap-1 px-2 py-1 rounded hover:bg-destructive/20 text-muted-foreground hover:text-red-400 text-xs font-medium transition-colors"
                        title="Uninstall Service"
                      >
                        <Trash2 className={`h-3.5 w-3.5 ${deletingName === service.name ? 'animate-spin' : ''}`} />
                      </button>
                    </div>
                  </td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
};
