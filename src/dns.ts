import { resolve4, resolve6 } from "node:dns/promises";
import { isIP } from "node:net";
import type { Logger } from "./log.ts";
import type { SettingsService } from "./settings.ts";

/** How long a detected answer is reused before DNS is asked again. */
const CACHE_TTL = 5 * 60 * 1000;

/**
 * How long detection may take before it counts as having found nothing.
 *
 * A resolver that black-holes queries would otherwise stall the dialog staff
 * are staring at for the resolver's own retry budget, which is tens of seconds.
 */
const DETECT_TIMEOUT = 3000;

export type DnsRecordType = "A" | "AAAA" | "CNAME";

export interface DnsRecord {
  type: DnsRecordType;
  value: string;
}

export interface DnsTarget {
  /** The records that point a hostname at this server. Empty if unknown. */
  records: DnsRecord[];
  /** `settings` when an admin set the target, `dashboard_host` when detected. */
  source: "settings" | "dashboard_host";
}

/** The slice of `node:dns` this needs, so tests can supply their own. */
export interface DnsResolver {
  resolve4(hostname: string): Promise<string[]>;
  resolve6(hostname: string): Promise<string[]>;
}

/** The record that points a hostname at one target. */
export function dnsRecordFor(target: string): DnsRecord {
  switch (isIP(target)) {
    case 4:
      return { type: "A", value: target };
    case 6:
      return { type: "AAAA", value: target };
    default:
      return { type: "CNAME", value: target };
  }
}

/**
 * What staff should point a hostname at, for the dashboard to show them.
 *
 * Admins can spell it out in the `dns_targets` setting, but they rarely have
 * to: whatever the dashboard hostname resolves to is by definition an address
 * that reaches this server, since that is how they got to the dashboard. So
 * the default is to resolve `DASHBOARD_HOST` and repeat its answer.
 *
 * That inherits the dashboard hostname's quirks — behind a CDN it reports the
 * CDN's addresses — which is why the setting exists to override it.
 */
export class DnsTargets {
  private readonly dashboardHost: string;
  private readonly settings: SettingsService;
  private readonly log: Logger;
  private readonly resolver: DnsResolver;
  private cache?: { records: DnsRecord[]; at: number };

  constructor(opts: {
    dashboardHost: string;
    settings: SettingsService;
    log: Logger;
    resolver?: DnsResolver;
  }) {
    this.dashboardHost = opts.dashboardHost;
    this.settings = opts.settings;
    this.log = opts.log;
    this.resolver = opts.resolver ?? { resolve4, resolve6 };
  }

  async target(): Promise<DnsTarget> {
    const configured = this.settings.get("dns_targets");
    if (configured.length) {
      return { records: configured.map(dnsRecordFor), source: "settings" };
    }

    return { records: await this.detect(), source: "dashboard_host" };
  }

  /** Addresses the dashboard hostname resolves to, cached including misses. */
  private async detect(): Promise<DnsRecord[]> {
    if (this.cache && Date.now() - this.cache.at < CACHE_TTL) {
      return this.cache.records;
    }

    // Either family may be absent — an IPv4-only host answers NODATA for AAAA —
    // so a failure of one is not a failure of the lookup.
    const addresses = await withTimeout(
      Promise.all([
        this.resolver.resolve4(this.dashboardHost).catch(() => []),
        this.resolver.resolve6(this.dashboardHost).catch(() => []),
      ]).then(([v4, v6]) => [...v4, ...v6]),
      DETECT_TIMEOUT,
      [],
    );

    if (!addresses.length) {
      this.log.debug("dns_target_undetected", { dashboard_host: this.dashboardHost });
    }

    const records = addresses.map(dnsRecordFor);
    this.cache = { records, at: Date.now() };
    return records;
  }
}

async function withTimeout<T>(promise: Promise<T>, ms: number, fallback: T): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;

  try {
    return await Promise.race([
      promise,
      new Promise<T>((resolve) => {
        timer = setTimeout(() => resolve(fallback), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
