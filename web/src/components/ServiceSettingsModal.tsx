import React, { useState } from 'react';
import { Settings, X } from 'lucide-react';
import { api } from '../api/client';
import { changedSettings, describeSource, SettingsDraft, toDraft } from '../lib/settings';
import { ServiceItem } from '../types/amoeba';
import { SettingsFields } from './SettingsFields';

/** Changes how an installed app runs. Saved settings are kept when the app
 *  is upgraded. */
export const ServiceSettingsModal: React.FC<{
  service: ServiceItem | null;
  onClose: () => void;
  onSaved: () => void;
}> = ({ service, onClose, onSaved }) => {
  const [draft, setDraft] = useState<SettingsDraft | null>(service?.settings ? toDraft(service.settings) : null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!service) return null;
  const base = service.settings ?? {};
  let pending: ReturnType<typeof changedSettings> = {};
  let draftError: string | null = null;
  try {
    pending = draft ? changedSettings(base, draft) : {};
  } catch (err: any) {
    draftError = err.message;
  }
  const restarts = pending.memory !== undefined || pending.cpu_cores !== undefined;
  const nothingChanged = Object.keys(pending).length === 0;

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await api.updateSettings(service.name, pending);
      onSaved();
      onClose();
    } catch (err: any) {
      setError(err.message || 'Could not save settings');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-4 overflow-y-auto">
      <div className="w-full max-w-lg rounded-xl border border-border bg-card p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-200 my-8">
        <div className="flex items-center justify-between pb-4 border-b border-border">
          <div className="flex items-center gap-2.5">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-blue-500/10 text-blue-400 border border-blue-500/20">
              <Settings className="h-5 w-5" />
            </div>
            <div>
              <h3 className="text-base font-semibold text-foreground">{service.name} settings</h3>
              <p className="text-xs text-muted-foreground">From {describeSource(service.source)}. Kept when it is upgraded.</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary/60 transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="py-4">
          {draft ? (
            <SettingsFields draft={draft} onChange={setDraft} />
          ) : (
            <p className="text-xs text-muted-foreground">This gateway did not report the service's settings.</p>
          )}
        </div>

        {(draftError || error) && (
          <div className="mb-3 rounded-lg bg-destructive/10 border border-destructive/20 p-3 text-xs text-destructive-foreground">
            {draftError || error}
          </div>
        )}
        {restarts && !draftError && (
          <p className="mb-3 text-[11px] text-amber-400">
            Changing memory or CPU restarts the app; open connections drop.
          </p>
        )}

        <div className="flex items-center justify-end gap-2 pt-4 border-t border-border">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3.5 py-2 text-xs font-medium text-muted-foreground hover:text-foreground hover:bg-secondary transition-colors"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={save}
            disabled={saving || nothingChanged || Boolean(draftError)}
            className="rounded-md bg-primary px-4 py-2 text-xs font-semibold text-primary-foreground shadow-sm hover:bg-primary/90 transition-colors disabled:opacity-50"
          >
            {saving ? 'Saving...' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  );
};
