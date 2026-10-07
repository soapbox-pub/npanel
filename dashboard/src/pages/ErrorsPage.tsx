import { useSeoMeta } from '@unhead/react';
import { Bug, ChevronRight } from 'lucide-react';
import { Link, useParams, useSearchParams } from 'react-router-dom';

import { PageHeader } from '@/components/PageHeader';
import { SiteIcon } from '@/components/SiteIcon';
import { Sparkline } from '@/components/Sparkline';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { useIssues } from '@/hooks/useErrors';
import { useHost, useUpdateHost } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { IssueStatus, ListedIssue } from '@/lib/api-types';
import { compactNumber, fullDate, timeAgo } from '@/lib/format';
import { cn } from '@/lib/utils';

const STATUSES: { id: IssueStatus; label: string }[] = [
  { id: 'unresolved', label: 'Unresolved' },
  { id: 'resolved', label: 'Resolved' },
  { id: 'ignored', label: 'Ignored' },
];

function parseStatus(value: string | null): IssueStatus {
  return STATUSES.some((s) => s.id === value) ? (value as IssueStatus) : 'unresolved';
}

/**
 * A site's errors, grouped into issues the way Sentry groups them, the most
 * recently seen first. The state shown is in the URL, so a link to the
 * resolved ones stays one.
 */
export function ErrorsPage() {
  const hostname = useParams().hostname ?? '';
  const host = useHost(hostname);
  const [search, setSearch] = useSearchParams();
  const status = parseStatus(search.get('status'));
  const issues = useIssues(hostname, status);

  useSeoMeta({ title: `${hostname} errors — npanel` });

  const back = { to: `/hosts/${hostname}`, label: hostname };

  if (host.isLoading) return <Skeleton className="h-96 w-full rounded-2xl" />;
  if (host.isError || !host.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={back} title="Site not found" />
        <p className="text-muted-foreground">
          {host.error instanceof Error ? host.error.message : 'There is no site by that name.'}
        </p>
      </div>
    );
  }

  const counts = issues.data?.pages[0]?.counts;
  const list = issues.data?.pages.flatMap((page) => page.issues) ?? [];

  return (
    <div className="space-y-6">
      <PageHeader
        back={back}
        icon={<SiteIcon hostname={hostname} src={host.data.preview.icon} className="size-10" />}
        title="Errors"
      />

      {!host.data.errors && <ReportingOff hostname={hostname} />}

      <div role="tablist" aria-label="Issues by status" className="inline-flex rounded-full bg-card p-1 surface">
        {STATUSES.map((s) => (
          <button
            key={s.id}
            type="button"
            role="tab"
            aria-selected={status === s.id}
            onClick={() =>
              setSearch((current) => {
                const next = new URLSearchParams(current);
                if (s.id === 'unresolved') next.delete('status');
                else next.set('status', s.id);
                return next;
              })
            }
            className={cn(
              'rounded-full px-3 py-1 text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden',
              status === s.id ? 'bg-primary text-primary-foreground' : 'text-muted-foreground hover:text-foreground',
            )}
          >
            {s.label}
            {counts && <span className="ml-1.5 tabular-nums opacity-70">{compactNumber(counts[s.id])}</span>}
          </button>
        ))}
      </div>

      {issues.isLoading ? (
        <Skeleton className="h-64 rounded-2xl" />
      ) : issues.isError ? (
        <p className="rounded-xl bg-destructive/10 p-4 text-sm text-destructive">
          {issues.error instanceof Error ? issues.error.message : "Couldn't load this site's errors."}
        </p>
      ) : !list.length ? (
        <Empty status={status} />
      ) : (
        <ul className="overflow-hidden rounded-2xl bg-card surface">
          {list.map((issue) => (
            <IssueRow key={issue.id} hostname={hostname} issue={issue} />
          ))}
        </ul>
      )}

      {issues.hasNextPage && (
        <div className="flex justify-center">
          <Button variant="outline" disabled={issues.isFetchingNextPage} onClick={() => issues.fetchNextPage()}>
            {issues.isFetchingNextPage ? 'Loading…' : 'Show more'}
          </Button>
        </div>
      )}
    </div>
  );
}

function IssueRow({ hostname, issue }: { hostname: string; issue: ListedIssue }) {
  return (
    <li className="border-t first:border-t-0">
      <Link
        to={`/hosts/${hostname}/errors/${issue.id}`}
        className="flex items-center gap-4 px-4 py-3 transition-colors hover:bg-secondary/50 focus-visible:bg-secondary/50 focus-visible:outline-hidden"
      >
        <div className="min-w-0 flex-1 space-y-0.5">
          <p className="truncate text-sm font-medium">{issue.title}</p>
          <p className="truncate font-mono text-xs text-muted-foreground">{issue.culprit || '—'}</p>
          <p className="text-xs text-muted-foreground">
            <span title={fullDate(issue.last_seen)}>{timeAgo(issue.last_seen)}</span>
            {' · '}
            <span title={fullDate(issue.first_seen)}>first {timeAgo(issue.first_seen)}</span>
          </p>
        </div>
        <Sparkline values={issue.hours} className="hidden h-8 w-24 shrink-0 text-destructive sm:block" />
        <div className="w-16 shrink-0 text-right">
          <p className="text-sm font-semibold tabular-nums">{compactNumber(issue.events)}</p>
          <p className="text-xs text-muted-foreground">{issue.events === 1 ? 'event' : 'events'}</p>
        </div>
        <div className="w-16 shrink-0 text-right">
          <p className="text-sm font-semibold tabular-nums">{compactNumber(issue.visitors)}</p>
          <p className="text-xs text-muted-foreground">{issue.visitors === 1 ? 'visitor' : 'visitors'}</p>
        </div>
        <ChevronRight className="size-4 shrink-0 text-muted-foreground/60" aria-hidden />
      </Link>
    </li>
  );
}

function Empty({ status }: { status: IssueStatus }) {
  const text = {
    unresolved: 'Nothing unresolved. Errors visitors hit show up here.',
    resolved: 'No resolved issues.',
    ignored: 'No ignored issues.',
  }[status];

  return (
    <div className="flex flex-col items-center gap-3 rounded-2xl border-2 border-dashed px-8 py-16 text-center">
      <Bug className="size-7 text-muted-foreground" aria-hidden />
      <p className="text-sm text-muted-foreground">{text}</p>
    </div>
  );
}

/** Shown while a site isn't reporting: what it would take, and the switch that does it. */
function ReportingOff({ hostname }: { hostname: string }) {
  const update = useUpdateHost();
  const { toast } = useToast();

  return (
    <div className="flex flex-wrap items-center justify-between gap-4 rounded-2xl bg-card p-4 surface">
      <div className="space-y-0.5">
        <p className="text-sm font-medium">This site isn&apos;t reporting errors.</p>
        <p className="text-xs text-muted-foreground">
          Its pages load a small reporter that sends what they throw here, and nowhere else.
        </p>
      </div>
      <label className="flex items-center gap-3 text-sm">
        <Switch
          disabled={update.isPending}
          onCheckedChange={async (errors) => {
            try {
              await update.mutateAsync({ hostname, patch: { errors } });
              toast({ title: 'Reporting errors on this site' });
            } catch (error) {
              toast({
                title: "Couldn't turn error reporting on",
                description: error instanceof Error ? error.message : String(error),
                variant: 'destructive',
              });
            }
          }}
        />
        Report errors
      </label>
    </div>
  );
}

export default ErrorsPage;
