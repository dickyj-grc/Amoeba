/** How the console shows a service, derived from the gateway's status. */
export interface AppLifecycleSnapshot {
  /** Warm: running. Warming: being prepared. Cold: scaled to zero or not
   *  started yet. Error: preparation failed; `error` says why. */
  phase: 'Cold' | 'Warming' | 'Warm' | 'Error';
  active_requests: number;
  error?: string;
}

export interface ServiceItem {
  name: string;
  state: AppLifecycleSnapshot;
  port?: number;
  cooldown_seconds?: number;
  public?: boolean;
  memory?: string;
  cpu_cores?: number;
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
