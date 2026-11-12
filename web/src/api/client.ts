import {
  AppsListResponse,
  AppStatusResponse,
  IssuedToken,
  LibraryEntry,
  LoginResponse,
  ServiceItem,
  ServiceTokenResponse,
} from '../types/amoeba';
import { toServiceItem } from '../lib/services';

class AmoebaApiClient {
  private get baseUrl(): string {
    return '';
  }

  public getToken(): string | null {
    return sessionStorage.getItem('amoeba_token') || localStorage.getItem('amoeba_token');
  }

  public setToken(token: string, persist: boolean = false): void {
    if (persist) {
      localStorage.setItem('amoeba_token', token);
    } else {
      sessionStorage.setItem('amoeba_token', token);
    }
  }

  public clearToken(): void {
    sessionStorage.removeItem('amoeba_token');
    localStorage.removeItem('amoeba_token');
  }

  private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
    const token = this.getToken();
    const headers: Record<string, string> = {
      ...(options.headers as Record<string, string>),
    };

    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }

    if (!(options.body instanceof FormData) && !headers['Content-Type']) {
      headers['Content-Type'] = 'application/json';
    }

    const response = await fetch(`${this.baseUrl}${path}`, {
      ...options,
      headers,
    });

    if (response.status === 401) {
      this.clearToken();
      window.dispatchEvent(new Event('amoeba:unauthorized'));
      throw new Error('Unauthorized or expired session');
    }

    if (!response.ok) {
      const errorText = await response.text();
      throw new Error(errorText || `Request failed with status ${response.status}`);
    }

    if (response.status === 204) {
      return null as T;
    }

    return response.json();
  }

  public async login(password: string, username: string = 'admin'): Promise<LoginResponse> {
    const res = await this.request<LoginResponse>('/auth/login', {
      method: 'POST',
      body: JSON.stringify({ username, password }),
    });
    this.setToken(res.token);
    return res;
  }

  public async listApps(): Promise<ServiceItem[]> {
    const list = await this.request<AppsListResponse>('/admin/apps', {
      method: 'GET',
    });

    if (!list || !list.apps) return [];

    // Sorted so rows keep their place between refreshes.
    const names = [...list.apps].sort();
    const statuses = await Promise.all(
      names.map(async (name) => {
        try {
          const status = await this.request<AppStatusResponse>(`/admin/apps/${encodeURIComponent(name)}`, {
            method: 'GET',
          });
          return toServiceItem(status);
        } catch (e) {
          return {
            name,
            state: { phase: 'Error' as const, active_requests: 0, error: 'status unavailable' },
          };
        }
      })
    );

    return statuses;
  }

  public async uninstallApp(name: string): Promise<void> {
    await this.request<void>(`/admin/apps/${encodeURIComponent(name)}`, {
      method: 'DELETE',
    });
  }

  public async issueToken(
    name: string,
    options: { orgId?: string; ttlSeconds?: number; label?: string } = {},
  ): Promise<ServiceTokenResponse> {
    return this.request<ServiceTokenResponse>(`/admin/apps/${encodeURIComponent(name)}/token`, {
      method: 'POST',
      body: JSON.stringify({
        org_id: options.orgId || undefined,
        ttl_seconds: options.ttlSeconds,
        label: options.label || undefined,
      }),
    });
  }

  public async listLibrary(): Promise<LibraryEntry[]> {
    return this.request<LibraryEntry[]>('/admin/library');
  }

  /** Installs a library app; `name` and `public` override the app's own. */
  public async installFromLibrary(
    id: string,
    options: { name?: string; public?: boolean; env?: Record<string, string>; secrets?: Record<string, string> },
  ): Promise<void> {
    await this.request(`/admin/library/${encodeURIComponent(id)}/install`, {
      method: 'POST',
      body: JSON.stringify({
        name: options.name || undefined,
        public: options.public,
        env: options.env ?? {},
        secrets: options.secrets ?? {},
      }),
    });
  }

  public async listTokens(name: string): Promise<IssuedToken[]> {
    return this.request<IssuedToken[]>(`/admin/apps/${encodeURIComponent(name)}/tokens`);
  }

  public async revokeToken(name: string, jti: string): Promise<void> {
    await this.request<null>(
      `/admin/apps/${encodeURIComponent(name)}/tokens/${encodeURIComponent(jti)}`,
      { method: 'DELETE' },
    );
  }

  public async testEndpoint(path: string, method: string = 'GET', body?: string): Promise<{ status: number; data: any; durationMs: number }> {
    const start = performance.now();
    const token = this.getToken();
    const headers: Record<string, string> = {};
    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }
    if (body) {
      headers['Content-Type'] = 'application/json';
    }

    const res = await fetch(path, {
      method,
      headers,
      body,
    });
    const durationMs = Math.round(performance.now() - start);

    let data: any;
    const text = await res.text();
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }

    return {
      status: res.status,
      data,
      durationMs,
    };
  }
}

export const api = new AmoebaApiClient();
