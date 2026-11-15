/** How the console shows a service, derived from the gateway's status. */
export interface AppLifecycleSnapshot {
  /** Warm: running. Warming: being prepared. Cold: scaled to zero or not
   *  started yet. Error: preparation failed; `error` says why. */
  phase: 'Cold' | 'Warming' | 'Warm' | 'Error';
  active_requests: number;
  error?: string;
}

/** Where an installed app came from. */
export interface AppSource {
  kind: 'library' | 'package';
  id?: string;
  version?: string;
}

/** How a site runs an app: who may call it, its memory and CPU, and when it
 *  idles. `cooldown_seconds: 0` keeps it running. */
export interface AppSettings {
  permissions?: Record<string, string[]>;
  public?: boolean;
  memory?: string;
  cpu_cores?: number;
  cooldown_seconds?: number;
}

export interface ServiceItem {
  name: string;
  state: AppLifecycleSnapshot;
  port?: number;
  cooldown_seconds?: number;
  public?: boolean;
  memory?: string;
  cpu_cores?: number;
  interface?: AppInterface;
  source?: AppSource;
  /** Current settings, every field set. */
  settings?: AppSettings;
}

export interface AppsListResponse {
  apps: string[];
}

/** `GET /admin/apps/<name>` as the gateway sends it. */
export interface AppStatusResponse {
  name: string;
  state: {
    state: 'installed' | 'pulling' | 'ready' | 'error';
    message: string | null;
    updated_at: number;
  };
  runtime?: {
    running: boolean | null;
    active_requests: number;
    last_request_at: number | null;
    port: number;
    public: boolean;
  };
  interface?: AppInterface;
  source?: AppSource;
  settings?: AppSettings;
}

export interface LoginResponse {
  token: string;
  expires_at: number;
}

export interface ServiceTokenResponse {
  token: string;
  role: string;
  service: string;
  org_id: string | null;
  jti: string;
  label: string;
  /** Unix seconds. */
  expires_at: number;
}

/** An issued service token as listed by the gateway: metadata only. */
export interface IssuedToken {
  jti: string;
  service: string;
  label: string;
  org_id: string | null;
  role: string;
  issued_at: number;
  expires_at: number;
}

export interface InspectRepoResponse {
  detected_framework: string;
  detected_port: number;
  has_dockerfile: boolean;
  env_vars: {
    name: string;
    default_value: string | null;
    is_required: boolean;
    is_secret: boolean;
    description?: string;
  }[];
}

/** How to call a service, from its app package (`interface` in amoeba.yaml). */
export interface AppInterface {
  type: 'mcp' | 'openapi' | 'http' | 'cdp';
  path?: string;
  summary?: string;
  docs?: string;
  try?: { method: string; path: string; body?: unknown };
}

export interface LibraryField {
  description: string;
  /** The installer must fill it in (no default to fall back to). */
  required: boolean;
  default?: string;
  /** Env vars only. */
  type?: 'string' | 'number' | 'integer' | 'boolean';
  options?: string[];
}

/** An app in the gateway's app library (`GET /admin/library`). */
export interface LibraryEntry {
  id: string;
  title: string;
  summary: string;
  tags: string[];
  images: string[];
  repos: string[];
  default_name: string;
  description: string;
  image?: string;
  command?: string[];
  port: number;
  public: boolean;
  local_upstream: boolean;
  permissions: Record<string, string[]>;
  memory?: string;
  cpu_cores?: number;
  /** Absent: keeps running. */
  cooldown_seconds?: number;
  interface?: AppInterface;
  env: Record<string, LibraryField>;
  secrets: Record<string, LibraryField>;
}
