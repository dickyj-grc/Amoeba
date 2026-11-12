import { AppStatusResponse, ServiceItem } from '../types/amoeba';

/** True when two service lists would render the same, so a refresh that
 *  changed nothing can keep the old array and skip a re-render. */
export function sameServices(a: ServiceItem[], b: ServiceItem[]): boolean {
  return a.length === b.length && JSON.stringify(a) === JSON.stringify(b);
}

/** Maps the gateway's status to how the table shows it. */
export function toServiceItem(status: AppStatusResponse): ServiceItem {
  const runtime = status.runtime;
  const lifecycle = status.state.state;
  const phase =
    lifecycle === 'error'
      ? 'Error'
      : lifecycle !== 'ready'
        ? 'Warming'
        : runtime?.running
          ? 'Warm'
          : 'Cold';
  return {
    name: status.name,
    state: {
      phase,
      active_requests: runtime?.active_requests ?? 0,
      ...(lifecycle === 'error' && status.state.message ? { error: status.state.message } : {}),
    },
    ...(runtime ? { port: runtime.port, public: runtime.public } : {}),
    ...(status.interface ? { interface: status.interface } : {}),
  };
}
