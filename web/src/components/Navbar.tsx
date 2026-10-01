import React from 'react';
import { Activity, LogOut, Plus, ShieldCheck, Terminal } from 'lucide-react';

interface NavbarProps {
  onOpenDeploy: () => void;
  onLogout: () => void;
  onOpenCli: () => void;
  isAuthenticated: boolean;
}

export const Navbar: React.FC<NavbarProps> = ({
  onOpenDeploy,
  onLogout,
  onOpenCli,
  isAuthenticated,
}) => {
  return (
    <header className="sticky top-0 z-40 w-full border-b border-border/80 bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
      <div className="max-w-7xl mx-auto flex h-16 items-center justify-between px-4 sm:px-6 lg:px-8">
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-gradient-to-br from-blue-500 to-indigo-600 shadow-md shadow-blue-500/20 text-white font-bold text-lg">
            A
          </div>
          <div>
            <div className="flex items-center gap-2">
              <span className="font-bold tracking-tight text-foreground text-lg">Amoeba</span>
              <span className="inline-flex items-center rounded-full bg-blue-500/10 px-2 py-0.5 text-xs font-medium text-blue-400 border border-blue-500/20">
                Control Plane
              </span>
            </div>
            <p className="text-xs text-muted-foreground hidden sm:block">Scale to zero • Zero trust • Zero egress</p>
          </div>
        </div>

        <div className="flex items-center gap-3">
          {isAuthenticated ? (
            <>
              <button
                onClick={onOpenCli}
                className="hidden sm:inline-flex items-center gap-1.5 rounded-md bg-secondary/80 hover:bg-secondary px-3 py-1.5 text-xs font-medium text-secondary-foreground transition-colors border border-border"
                title="View Amoeba CLI & MCP instructions"
              >
                <Terminal className="h-3.5 w-3.5 text-muted-foreground" />
                <span>CLI & MCP</span>
              </button>

              <button
                onClick={onOpenDeploy}
                className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3.5 py-1.5 text-xs font-semibold text-primary-foreground shadow-sm hover:bg-primary/90 transition-colors"
              >
                <Plus className="h-3.5 w-3.5" />
                <span>Deploy Service</span>
              </button>

              <div className="h-4 w-px bg-border mx-1" />

              <div className="flex items-center gap-2">
                <span className="inline-flex items-center gap-1 text-xs text-emerald-400 font-mono bg-emerald-500/10 px-2 py-1 rounded border border-emerald-500/20">
                  <ShieldCheck className="h-3.5 w-3.5" />
                  admin
                </span>
                <button
                  onClick={onLogout}
                  className="rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary/60 transition-colors"
                  title="Logout"
                >
                  <LogOut className="h-4 w-4" />
                </button>
              </div>
            </>
          ) : (
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <Activity className="h-3.5 w-3.5 animate-pulse text-amber-500" />
              <span>Authentication required</span>
            </div>
          )}
        </div>
      </div>
    </header>
  );
};
