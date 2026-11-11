export interface AppLifecycleSnapshot {
  phase: 'Cold' | 'Warming' | 'Warm' | 'Cooling' | 'Error' | 'Installed';
  generation: number;
  observed_generation: number;
  ready: boolean;
  healthy: boolean;
  active_requests: number;
  last_activity?: string;
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

export interface AppStatusResponse {
  name: string;
  state: AppLifecycleSnapshot;
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
