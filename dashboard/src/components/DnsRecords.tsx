import { CopyButton } from '@/components/CopyButton';
import { Skeleton } from '@/components/ui/skeleton';
import { useDnsTarget } from '@/hooks/useNpanel';
import type { DnsRecord } from '@/lib/api-types';
import { isRootDomain, recordName } from '@shared/hostname';

/** One line of the table: a record, and the name field it goes in. */
interface Row {
  /** Absent when npanel can't tell which part of the hostname is the name. */
  name?: string;
  record: DnsRecord;
}

/**
 * The DNS records a domain needs before this gateway can answer for it —
 * the one step npanel can't take for anyone, and the one that silently
 * breaks everything downstream when it's skipped.
 *
 * `www` adds the row a redirect needs: the redirect is an HTTPS response like
 * any other, so `www` has to resolve here and get a certificate of its own.
 */
export function DnsRecords({ hostname, www = false }: { hostname: string; www?: boolean }) {
  const dns = useDnsTarget();

  if (dns.isLoading) return <Skeleton className="h-16 w-full rounded-xl" />;
  if (!dns.data) return null;

  const { records } = dns.data;
  const name = hostname.trim().toLowerCase();

  if (!records.length) {
    return (
      <p className="rounded-xl bg-warning/10 p-3 text-xs text-foreground">
        npanel doesn't know its own address yet. Ask an admin to set a DNS target in Settings.
      </p>
    );
  }

  // No name column until the public suffix list recognizes the hostname: a
  // guess at which part goes in a DNS provider's name field is worse than
  // leaving it to staff, who know their own zone.
  const label = name ? recordName(name) : undefined;
  const rows = records.flatMap((record): Row[] =>
    label
      ? [{ name: label, record }, ...(www && isRootDomain(name) ? [{ name: 'www', record }] : [])]
      : [{ name: undefined, record }],
  );

  return (
    <div className="rounded-xl bg-muted p-3">
      <p className="pb-2 text-xs text-muted-foreground">Add at your DNS provider:</p>
      <ul className="space-y-0.5">
        {rows.map((row) => (
          <li
            key={`${row.name}:${row.record.type}:${row.record.value}`}
            className="flex items-center gap-3 font-mono text-xs"
          >
            {row.name !== undefined && (
              <span className="w-14 shrink-0 truncate" title={row.name}>
                {row.name}
              </span>
            )}
            <span className="w-11 shrink-0 text-muted-foreground">{row.record.type}</span>
            <span className="min-w-0 flex-1 truncate">{row.record.value}</span>
            <CopyButton
              value={row.record.value}
              // The two rows of a www redirect carry the same value, so the name
              // is what tells their buttons apart.
              label={row.name ? `Copy ${row.record.value} for ${row.name}` : `Copy ${row.record.value}`}
              className="size-6"
            />
          </li>
        ))}
      </ul>
    </div>
  );
}
