import React from 'react';
import { Bot, Terminal, X } from 'lucide-react';

interface CliInfoModalProps {
  isOpen: boolean;
  onClose: () => void;
}

export const CliInfoModal: React.FC<CliInfoModalProps> = ({ isOpen, onClose }) => {
  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4">
      <div className="w-full max-w-2xl rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200 space-y-4">
        <div className="flex items-center justify-between pb-3 border-b border-border">
          <div className="flex items-center gap-2.5">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-indigo-500/10 text-indigo-400 border border-indigo-500/20">
              <Bot className="h-5 w-5" />
            </div>
            <div>
              <h3 className="text-base font-semibold text-foreground">AI & CLI Integration</h3>
              <p className="text-xs text-muted-foreground">Model Context Protocol (MCP) & Amoeba Admin CLI</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary/60 transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="space-y-4 text-xs">
          <div>
            <h4 className="font-semibold text-foreground mb-1 flex items-center gap-1.5">
              <Bot className="h-4 w-4 text-blue-400" />
              Configure AI MCP Server (Claude, Gemini, Cursor)
            </h4>
            <p className="text-muted-foreground mb-2">
              Add the Amoeba MCP server to your AI client configuration:
            </p>
            <pre className="p-3 rounded-lg bg-background border border-border font-mono text-[11px] overflow-x-auto text-blue-300">
{`{
  "mcpServers": {
    "amoeba": {
      "command": "/path/to/target/release/amoeba-mcp",
      "env": {
        "AMOEBA_URL": "http://localhost:8080",
        "AMOEBA_TOKEN": "<YOUR_ADMIN_JWT>"
      }
    }
  }
}`}
            </pre>
          </div>

          <div>
            <h4 className="font-semibold text-foreground mb-1 flex items-center gap-1.5">
              <Terminal className="h-4 w-4 text-emerald-400" />
              Amoeba Admin CLI
            </h4>
            <p className="text-muted-foreground mb-2">
              Use the built-in CLI to manage users, tokens, and scale status:
            </p>
            <pre className="p-3 rounded-lg bg-background border border-border font-mono text-[11px] overflow-x-auto text-emerald-300">
{`# Create user
cargo run --bin amoeba-admin -- create-user --username alice --roles admin

# Issue service token
cargo run --bin amoeba-admin -- token --username dev --role read --service pdf-parser`}
            </pre>
          </div>
        </div>
      </div>
    </div>
  );
};
