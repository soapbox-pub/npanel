import { Check, Copy } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useDnsTarget } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { DnsRecord } from '@/lib/api-types';

/**
 * The DNS records a hostname needs before this gateway can answer for it.
 *
 * Shown while staff are typing the hostname, because that is the one step
 * npanel cannot do for them — and the one that silently breaks everything
 * downstream, from certificates to the site itself, when it's skipped.
 */
export function DnsRecords({ hostname }: { hostname: string }) {
  const dns = useDnsTarget();

  if (dns.isLoading) return <Skeleton className="h-20 w-full" />;
  if (!dns.data) return null;

  const { records, source, dashboard_host } = dns.data;
  const name = hostname.trim().toLowerCase();

  return (
    <div className="space-y-2 rounded-lg border bg-muted/40 p-3">
      {records.length ? (
        <>
          <p className="text-xs text-muted-foreground">
            Create {records.length > 1 ? 'these records' : 'this record'} for{' '}
            <span className="font-medium text-foreground">{name || 'the hostname'}</span> at your DNS
            provider:
          </p>

          <ul className="space-y-1">
            {records.map((record) => (
              <RecordRow key={`${record.type}:${record.value}`} record={record} />
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
          Point the hostname at this server before it can be served. npanel couldn't work out its own
          address — {dashboard_host} doesn't resolve from here — so an admin needs to set the DNS target
          under Settings.
        </p>
      )}
    </div>
  );
}

function RecordRow({ record }: { record: DnsRecord }) {
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
      <span className="w-12 shrink-0 text-muted-foreground">{record.type}</span>
      <span className="truncate">{record.value}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="ml-auto size-6 shrink-0"
        onClick={copy}
        aria-label={`Copy ${record.value}`}
      >
        {copied ? <Check className="size-3" aria-hidden /> : <Copy className="size-3" aria-hidden />}
      </Button>
    </li>
  );
}
