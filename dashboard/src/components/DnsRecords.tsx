import { Check, Copy } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useDnsTarget } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { DnsRecord } from '@/lib/api-types';
import { isRootDomain, recordName } from '@shared/hostname';

/** One line of the table: a record, and the name field it goes in. */
interface Row {
  /** Absent when npanel can't tell which part of the hostname is the name. */
  name?: string;
  record: DnsRecord;
}

/**
 * The DNS records a domain needs before this gateway can answer for it.
 *
 * Shown while staff are typing the domain, because that is the one step
 * npanel cannot do for them — and the one that silently breaks everything
 * downstream, from certificates to the site itself, when it's skipped.
 *
 * `www` adds the second row that redirect needs. A redirect is still an HTTPS
 * response, so `www` has to resolve here and get a certificate of its own
 * before it can be told where to go — which makes this the moment to say so.
 */
export function DnsRecords({ hostname, www = false }: { hostname: string; www?: boolean }) {
  const dns = useDnsTarget();

  if (dns.isLoading) return <Skeleton className="h-20 w-full" />;
  if (!dns.data) return null;

  const { records, source, dashboard_host } = dns.data;
  const name = hostname.trim().toLowerCase();

  // No name column until the hostname is one the public suffix list recognizes:
  // a guess about which part of it goes in a DNS provider's name field is worse
  // than leaving the field to staff, who know their own zone.
  const label = name ? recordName(name) : undefined;
  const rows = records.flatMap((record): Row[] =>
    label
      ? [{ name: label, record }, ...(www && isRootDomain(name) ? [{ name: 'www', record }] : [])]
      : [{ name: undefined, record }],
  );

  return (
    <div className="space-y-2 rounded-lg border bg-muted/40 p-3">
      {records.length ? (
        <>
          <p className="text-xs text-muted-foreground">
            Create {rows.length > 1 ? 'these records' : 'this record'} for{' '}
            <span className="font-medium text-foreground">{name || 'the domain'}</span> at your DNS
            provider:
          </p>

          <ul className="space-y-1">
            {rows.map((row) => (
              <RecordRow key={`${row.name}:${row.record.type}:${row.record.value}`} {...row} />
            ))}
          </ul>

          {source === 'dashboard_host' && (
            <p className="text-xs text-muted-foreground">
              Detected from {dashboard_host}. An admin can set a different target under Settings.
            </p>
          )}
        </>
      ) : (
        <p className="text-xs text-muted-foreground">
          Point the domain at this server before it can be served. npanel couldn't work out its own
          address — {dashboard_host} doesn't resolve from here — so an admin needs to set the DNS target
          under Settings.
        </p>
      )}
    </div>
  );
}

function RecordRow({ name, record }: Row) {
  const [copied, setCopied] = useState(false);
  const { toast } = useToast();

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(record.value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard access needs a secure context, which a plain-HTTP dashboard isn't.
      toast({ title: "Couldn't copy", description: 'Select the value and copy it manually.' });
    }
  };

  return (
    <li className="flex items-center gap-2 font-mono text-xs">
      {name !== undefined && (
        <span className="w-16 shrink-0 truncate text-foreground" title={name}>
          {name}
        </span>
      )}
      <span className="w-12 shrink-0 text-muted-foreground">{record.type}</span>
      <span className="truncate">{record.value}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="ml-auto size-6 shrink-0"
        onClick={copy}
        // The two rows of a www redirect carry the same value, so the name is
        // what tells their buttons apart.
        aria-label={name ? `Copy ${record.value} for ${name}` : `Copy ${record.value}`}
      >
        {copied ? <Check className="size-3" aria-hidden /> : <Copy className="size-3" aria-hidden />}
      </Button>
    </li>
  );
}
