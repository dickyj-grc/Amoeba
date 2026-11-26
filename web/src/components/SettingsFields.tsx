import React from 'react';
import { AlertTriangle } from 'lucide-react';
import { SettingsDraft } from '../lib/settings';
import { ServiceItem } from '../types/amoeba';

const inputClass =
  'w-full rounded-md border border-input bg-background px-3 py-1.5 text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring';

/** How an app runs on this site: who may call it, memory, CPU, idle time.
 *  The same fields before deploy ("Adjust") and after (Settings). */
export const SettingsFields: React.FC<{
  draft: SettingsDraft;
  onChange: (draft: SettingsDraft) => void;
  /** Off where the form already has its own public checkbox. */
  showPublic?: boolean;
}> = ({ draft, onChange, showPublic = true }) => (
  <div className="space-y-3 text-xs">
    <div>
      <span className="block font-medium text-foreground mb-1">Who can call it</span>
      <div className="grid grid-cols-[4rem_1fr] items-center gap-x-2 gap-y-1.5">
        {Object.entries(draft.permissions).map(([op, roles]) => (
          <React.Fragment key={op}>
            <label className="font-mono text-muted-foreground">{op}</label>
            <input
              type="text"
              value={roles}
              placeholder="nobody"
              onChange={(e) => onChange({ ...draft, permissions: { ...draft.permissions, [op]: e.target.value } })}
              className={inputClass}
            />
          </React.Fragment>
        ))}
      </div>
      <p className="mt-1 text-[10px] text-muted-foreground">
        Roles, comma-separated. Tokens from the Token button carry <span className="font-mono">mcp:&lt;name&gt;</span>.
      </p>
    </div>
    {showPublic && (
      <label className="flex items-center gap-2 cursor-pointer">
        <input
          type="checkbox"
          checked={draft.public}
          onChange={(e) => onChange({ ...draft, public: e.target.checked })}
          className="rounded border-input h-4 w-4 bg-background"
        />
        <span className="font-medium text-foreground">Public (no login needed)</span>
      </label>
    )}
    <div className="grid grid-cols-3 gap-3">
      <label className="block">
        <span className="block font-medium text-foreground mb-1">Memory</span>
        <input
          type="text"
          value={draft.memory}
          placeholder="e.g. 512Mi"
          onChange={(e) => onChange({ ...draft, memory: e.target.value })}
          className={inputClass}
        />
      </label>
      <label className="block">
        <span className="block font-medium text-foreground mb-1">CPU cores</span>
        <input
          type="number"
          min={0}
          value={draft.cpu}
          placeholder="no limit"
          onChange={(e) => onChange({ ...draft, cpu: e.target.value })}
          className={inputClass}
        />
      </label>
      <label className="block">
        <span className="block font-medium text-foreground mb-1">Idle stop (s)</span>
        <input
          type="number"
          min={0}
          value={draft.cooldown}
          onChange={(e) => onChange({ ...draft, cooldown: e.target.value })}
          className={inputClass}
        />
      </label>
    </div>
    <label className="block">
      <span className="block font-medium text-foreground mb-1">Requests per minute, per caller</span>
      <input
        type="number"
        min={0}
        value={draft.rateLimit}
        placeholder="gateway default (600)"
        onChange={(e) => onChange({ ...draft, rateLimit: e.target.value })}
        className={inputClass}
      />
    </label>
    <p className="text-[10px] text-muted-foreground">Idle stop 0 keeps the app running. Requests per minute 0 is unlimited.</p>
  </div>
);

/** Shown when the chosen name already belongs to a service. */
export const NameConflict: React.FC<{
  name: string;
  existing: ServiceItem;
  /** Same app: deploying upgrades it and keeps its settings. */
  sameApp: boolean;
  from: string;
  replace: boolean;
  suggestion: string;
  onReplace: () => void;
  onRename: (name: string) => void;
}> = ({ name, sameApp, from, replace, suggestion, onReplace, onRename }) => (
  <div className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-[11px] space-y-1.5">
    <div className="flex items-start gap-1.5 text-foreground">
      <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-400" />
      <span>
        <span className="font-mono">{name}</span> already exists, from {from}.{' '}
        {sameApp
          ? 'Deploying upgrades it and keeps its settings.'
          : replace
            ? 'Deploying replaces it; its settings are not kept.'
            : 'Pick another name, or replace it.'}
      </span>
    </div>
    {!sameApp && !replace && (
      <div className="flex gap-2">
        <button
          type="button"
          onClick={() => onRename(suggestion)}
          className="rounded px-2 py-1 font-medium text-blue-400 hover:bg-blue-500/10"
        >
          Use {suggestion}
        </button>
        <button
          type="button"
          onClick={onReplace}
          className="rounded px-2 py-1 font-medium text-red-400 hover:bg-red-500/10"
        >
          Replace it
        </button>
      </div>
    )}
  </div>
);
