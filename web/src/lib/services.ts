import { ServiceItem } from '../types/amoeba';

/** True when two service lists would render the same, so a refresh that
 *  changed nothing can keep the old array and skip a re-render. */
export function sameServices(a: ServiceItem[], b: ServiceItem[]): boolean {
  return a.length === b.length && JSON.stringify(a) === JSON.stringify(b);
}
