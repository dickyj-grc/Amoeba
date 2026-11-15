import { AppSettings, AppSource, ServiceItem } from '../types/amoeba';

/** The settings form's editable values, as typed. */
export interface SettingsDraft {
  /** Operation to a comma-separated role list. */
  permissions: Record<string, string>;
  public: boolean;
  memory: string;
  cpu: string;
  /** Seconds; "0" keeps the app running. */
  cooldown: string;
}

const OPERATIONS = ['read', 'add', 'update', 'delete'];

export function toDraft(settings: AppSettings): SettingsDraft {
  const permissions: Record<string, string> = {};
  // The standard operations always show, so roles can be granted to them.
  for (const op of [...OPERATIONS, ...Object.keys(settings.permissions ?? {})]) {
    permissions[op] = (settings.permissions?.[op] ?? []).join(', ');
  }
  return {
    permissions,
    public: settings.public ?? false,
    memory: settings.memory ?? '',
    cpu: settings.cpu_cores ? String(settings.cpu_cores) : '',
    cooldown: String(settings.cooldown_seconds ?? 0),
  };
}

function parseRoles(text: string): string[] {
  return text
    .split(',')
    .map((r) => r.trim())
    .filter(Boolean);
}

function samePermissions(a: Record<string, string[]>, b: Record<string, string[]>): boolean {
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  return [...keys].every((k) => (a[k] ?? []).join(',') === (b[k] ?? []).join(','));
}

/** Only what the draft changes from `base`, ready to send; `{}` when nothing
 *  changed. Throws on an invalid number. */
export function changedSettings(base: AppSettings, draft: SettingsDraft): AppSettings {
  const changed: AppSettings = {};
  const permissions: Record<string, string[]> = {};
  for (const [op, roles] of Object.entries(draft.permissions)) {
    const list = parseRoles(roles);
    if (list.length > 0) permissions[op] = list;
  }
  if (!samePermissions(permissions, base.permissions ?? {})) changed.permissions = permissions;
  if (draft.public !== (base.public ?? false)) changed.public = draft.public;
  if (draft.memory.trim() !== (base.memory ?? '')) {
    if (!draft.memory.trim()) throw new Error('Memory cannot be removed, only changed (e.g. 512Mi).');
    changed.memory = draft.memory.trim();
  }
  const cpu = draft.cpu.trim() === '' ? 0 : Number(draft.cpu);
  if (!Number.isInteger(cpu) || cpu < 0) throw new Error('CPU cores must be a whole number.');
  if (cpu !== (base.cpu_cores ?? 0)) changed.cpu_cores = cpu;
  const cooldown = Number(draft.cooldown || '0');
  if (!Number.isInteger(cooldown) || cooldown < 0) throw new Error('Idle shutdown must be whole seconds.');
  if (cooldown !== (base.cooldown_seconds ?? 0)) changed.cooldown_seconds = cooldown;
  return changed;
}

/** Console tokens carry `mcp:<name>`; keep a renamed app's grants pointing at it. */
export function renameTokenRole(settings: AppSettings, from: string, to: string): AppSettings {
  if (!settings.permissions || from === to) return settings;
  const permissions: Record<string, string[]> = {};
  for (const [op, roles] of Object.entries(settings.permissions)) {
    permissions[op] = roles.map((r) => (r === `mcp:${from}` ? `mcp:${to}` : r));
  }
  return { ...settings, permissions };
}

/** `name-2`, `name-3`, ... : the first name no service uses. */
export function freeName(name: string, taken: string[]): string {
  if (!taken.includes(name)) return name;
  for (let i = 2; ; i++) {
    const candidate = `${name}-${i}`;
    if (!taken.includes(candidate)) return candidate;
  }
}

/** Installing `source` as `existing` upgrades the same app rather than replacing another. */
export function isSameApp(existing: ServiceItem | undefined, source: AppSource): boolean {
  return existing?.source?.kind === source.kind && existing.source.id === source.id;
}

/** "the app library (lightpanda-mcp 1.0.0)" or "an uploaded package". */
export function describeSource(source?: AppSource): string {
  if (source?.kind === 'library' && source.id) {
    return `the app library (${source.id}${source.version ? ` ${source.version}` : ''})`;
  }
  return 'a package or image deploy';
}
