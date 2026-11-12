import React, { useEffect, useState } from 'react';
import { BookOpen, Check, Rocket, Search } from 'lucide-react';
import { api } from '../api/client';
import { LibraryEntry, LibraryField } from '../types/amoeba';
import { interfaceLabel, searchLibrary } from '../lib/library';
import { validateServiceName } from '../lib/deployPackage';

interface LibraryDeployProps {
  /** null while the library is loading. */
  entries: LibraryEntry[] | null;
  /** App to start with, e.g. from the Docker Image tab's "known app" hint. */
  initialId?: string;
  onDeployed: () => void;
  onCancel: () => void;
}

/** One env setting: a dropdown when the app lists its allowed values. */
const EnvInput: React.FC<{
  field: LibraryField;
  value: string;
  onChange: (value: string) => void;
  className: string;
}> = ({ field, value, onChange, className }) => {
  const options = field.options ?? (field.type === 'boolean' ? ['true', 'false'] : null);
  if (options) {
    return (
      <select value={value} onChange={(e) => onChange(e.target.value)} required={field.required} className={className}>
        {!field.required && <option value="">{field.default ? `app default (${field.default})` : 'not set'}</option>}
        {field.required && value === '' && <option value="" disabled>choose...</option>}
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    );
  }
  const numeric = field.type === 'number' || field.type === 'integer';
  return (
    <input
      type={numeric ? 'number' : 'text'}
      step={field.type === 'integer' ? 1 : 'any'}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      required={field.required}
      placeholder={field.default ? 'app default' : ''}
      className={className}
    />
  );
};

/** The App library tab: search ready-made apps, then deploy one with only the
 *  inputs it needs. Image, command, port and permissions come from the app. */
export const LibraryDeploy: React.FC<LibraryDeployProps> = ({ entries, initialId, onDeployed, onCancel }) => {
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<LibraryEntry | null>(null);
  const [name, setName] = useState('');
  const [isPublic, setIsPublic] = useState(false);
  const [env, setEnv] = useState<Record<string, string>>({});
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const select = (entry: LibraryEntry) => {
    setSelected(entry);
    setQuery(entry.title);
    setName(entry.default_name);
    setIsPublic(entry.public);
    setEnv(Object.fromEntries(Object.entries(entry.env).map(([k, f]) => [k, f.default ?? ''])));
    setSecrets(Object.fromEntries(Object.keys(entry.secrets).map((k) => [k, ''])));
    setError(null);
  };

  useEffect(() => {
    const initial = initialId && entries?.find((e) => e.id === initialId);
    if (initial) select(initial);
  }, [initialId, entries]);

  const results = entries ? searchLibrary(entries, selected ? '' : query) : [];

  const handleDeploy = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!selected) return;
    const nameError = validateServiceName(name.trim());
    if (nameError) {
      setError(nameError);
      return;
    }
    setIsSubmitting(true);
    setError(null);
    try {
      await api.installFromLibrary(selected.id, {
        name: name.trim(),
        public: isPublic,
        // Unset optional env vars fall back to the app's defaults.
        env: Object.fromEntries(Object.entries(env).filter(([, v]) => v !== '')),
        secrets: Object.fromEntries(Object.entries(secrets).filter(([, v]) => v !== '')),
      });
      onDeployed();
    } catch (err: any) {
      setError(err.message || 'Deployment failed');
    } finally {
      setIsSubmitting(false);
    }
  };

  const inputClass =
    'w-full rounded-md border border-input bg-background px-3 py-2 text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring';

  return (
    <form onSubmit={handleDeploy} className="space-y-4 text-xs">
      <div>
        <label className="block font-medium text-foreground mb-1">
          Search apps, or paste an image or GitHub URL
        </label>
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <input
            type="text"
            value={query}
            autoFocus
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(null);
            }}
            placeholder="lightpanda, lightpanda/browser:latest, github.com/lightpanda-io/browser"
            className={`${inputClass} pl-8`}
          />
        </div>
        {!selected && (
          <ul className="mt-2 max-h-56 overflow-y-auto divide-y divide-border rounded-md border border-border">
            {entries === null ? (
              <li className="px-3 py-2 text-muted-foreground">Loading the app library...</li>
            ) : results.length === 0 ? (
              <li className="px-3 py-2 text-muted-foreground">
                No app in the library matches. Use the Docker Image, Git or Dockerfile tab instead.
              </li>
            ) : (
              results.map((entry) => (
                <li key={entry.id}>
                  <button
                    type="button"
                    onClick={() => select(entry)}
                    className="w-full text-left px-3 py-2 hover:bg-secondary/60 transition-colors"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="font-medium text-foreground">{entry.title}</span>
                      {entry.interface && (
                        <span className="shrink-0 rounded bg-blue-500/10 px-1.5 py-0.5 text-[10px] text-blue-400">
                          {interfaceLabel(entry.interface.type)}
                        </span>
                      )}
                    </div>
                    <p className="text-[11px] text-muted-foreground">{entry.summary}</p>
                    <p className="text-[10px] text-muted-foreground font-mono">
                      {entry.image ?? 'compose app'}
                      {' · no build'}
                    </p>
                  </button>
                </li>
              ))
            )}
          </ul>
        )}
      </div>

      {selected && (
        <>
          <div className="rounded-lg border border-border bg-muted/20 p-3 space-y-1.5">
            <div className="flex items-center gap-2 text-foreground">
              <Check className="h-3.5 w-3.5 text-emerald-400" />
              <span className="font-semibold">{selected.title}</span>
              <span className="text-[10px] text-muted-foreground">from the Amoeba app library</span>
            </div>
            <p className="text-[11px] text-muted-foreground">{selected.description}</p>
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 font-mono text-[10px] text-muted-foreground">
              {selected.image && (
                <>
                  <dt>image</dt>
                  <dd className="break-all text-foreground">{selected.image}</dd>
                </>
              )}
              {selected.command && selected.command.length > 0 && (
                <>
                  <dt>command</dt>
                  <dd className="break-all text-foreground">{selected.command.join(' ')}</dd>
                </>
              )}
              <dt>port</dt>
              <dd className="text-foreground">{selected.port}</dd>
              {selected.interface && (
                <>
                  <dt>call it</dt>
                  <dd className="text-foreground">
                    {interfaceLabel(selected.interface.type)}
                    {selected.interface.path ? ` at /v1/${name || selected.default_name}${selected.interface.path}` : ''}
                  </dd>
                </>
              )}
            </dl>
            {selected.interface?.summary && (
              <p className="text-[11px] text-muted-foreground">{selected.interface.summary}</p>
            )}
            {selected.interface?.docs && (
              <a
                href={selected.interface.docs}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-1 text-[11px] text-blue-400 hover:underline"
              >
                <BookOpen className="h-3 w-3" />
                <span>Documentation</span>
              </a>
            )}
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div>
              <label className="block font-medium text-foreground mb-1">
                Service Name <span className="text-red-400">*</span>
              </label>
              <input type="text" value={name} onChange={(e) => setName(e.target.value)} className={inputClass} required />
              <p className="mt-1 text-[10px] text-muted-foreground">Reachable at /v1/{name || '<name>'}/</p>
            </div>
            <div className="flex items-center pt-5">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={isPublic}
                  onChange={(e) => setIsPublic(e.target.checked)}
                  className="rounded border-input text-primary focus:ring-ring h-4 w-4 bg-background"
                />
                <span className="font-medium text-foreground">Public API (No JWT Required)</span>
              </label>
            </div>
          </div>

          {(Object.keys(selected.env).length > 0 || Object.keys(selected.secrets).length > 0) && (
            <div className="space-y-3 pt-2 border-t border-border">
              <span className="font-medium text-foreground">Settings this app needs</span>
              {Object.entries(selected.secrets).map(([key, field]) => (
                <div key={key}>
                  <label className="block font-mono text-foreground mb-1">
                    {key} {field.required && <span className="text-red-400">*</span>}
                    <span className="ml-1 font-sans text-[10px] text-muted-foreground">secret</span>
                  </label>
                  <input
                    type="password"
                    value={secrets[key] ?? ''}
                    onChange={(e) => setSecrets({ ...secrets, [key]: e.target.value })}
                    required={field.required}
                    autoComplete="off"
                    className={inputClass}
                  />
                  {field.description && <p className="mt-1 text-[10px] text-muted-foreground">{field.description}</p>}
                </div>
              ))}
              {Object.entries(selected.env).map(([key, field]) => (
                <div key={key}>
                  <label className="block font-mono text-foreground mb-1">
                    {key} {field.required && <span className="text-red-400">*</span>}
                  </label>
                  <EnvInput
                    field={field}
                    value={env[key] ?? ''}
                    onChange={(value) => setEnv({ ...env, [key]: value })}
                    className={inputClass}
                  />
                  {field.description && <p className="mt-1 text-[10px] text-muted-foreground">{field.description}</p>}
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {error && (
        <div className="rounded-lg bg-destructive/10 border border-destructive/20 p-3 text-xs text-destructive-foreground">
          {error}
        </div>
      )}

      <div className="flex items-center justify-end gap-2 pt-4 border-t border-border">
        <button
          type="button"
          onClick={onCancel}
          className="rounded-md px-3.5 py-2 text-xs font-medium text-muted-foreground hover:text-foreground hover:bg-secondary transition-colors"
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={!selected || isSubmitting}
          className="inline-flex items-center gap-1.5 rounded-md bg-primary px-4 py-2 text-xs font-semibold text-primary-foreground shadow-sm hover:bg-primary/90 transition-colors disabled:opacity-50"
        >
          <Rocket className="h-3.5 w-3.5" />
          <span>{isSubmitting ? 'Deploying...' : 'Deploy to Amoeba'}</span>
        </button>
      </div>
    </form>
  );
};
