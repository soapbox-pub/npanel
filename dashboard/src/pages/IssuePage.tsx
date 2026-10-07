import { useSeoMeta } from '@unhead/react';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import { useState } from 'react';
import { Link, useParams, useSearchParams } from 'react-router-dom';

import { Row, Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { Sparkline } from '@/components/Sparkline';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useIssue, useIssueEvent, useSetIssueStatus } from '@/hooks/useErrors';
import { useToast } from '@/hooks/useToast';
import type { Breadcrumb, IssueDetail, IssueEvent, IssueStatus, ReportedException, StackFrame } from '@/lib/api-types';
import { compactNumber, fullDate, shorten, timeAgo } from '@/lib/format';
import { cn } from '@/lib/utils';

/**
 * One issue: how often it happens and to how many, and its events one at a
 * time — what was thrown, where, and what the visitor did first.
 */
export function IssuePage() {
  const { hostname = '', id: rawId = '' } = useParams();
  const id = Number(rawId);
  const issue = useIssue(hostname, id);
  const [search, setSearch] = useSearchParams();
  const chosen = search.get('event');
  const eventId = chosen !== null && Number.isFinite(Number(chosen)) ? Number(chosen) : (issue.data?.latest_event ?? null);
  const event = useIssueEvent(hostname, id, eventId);

  useSeoMeta({ title: issue.data ? `${issue.data.title} — npanel` : 'npanel' });

  const back = { to: `/hosts/${hostname}/errors`, label: 'Errors' };

  if (issue.isLoading) return <Skeleton className="h-96 w-full rounded-2xl" />;
  if (issue.isError || !issue.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={back} title="Issue not found" />
        <p className="text-muted-foreground">
          {issue.error instanceof Error ? issue.error.message : 'This site has no such issue.'}
        </p>
      </div>
    );
  }

  const data = issue.data;
  const go = (to: number | null) =>
    to !== null &&
    setSearch((current) => {
      const next = new URLSearchParams(current);
      next.set('event', String(to));
      return next;
    });

  return (
    <div className="space-y-8">
      <PageHeader
        back={back}
        title={<span className="[overflow-wrap:anywhere]">{data.title}</span>}
        subtitle={<span className="font-mono text-xs">{data.culprit}</span>}
        actions={<StatusButtons hostname={hostname} issue={data} />}
      />

      <Overview hostname={hostname} issue={data} />

      {eventId === null ? (
        <p className="text-sm text-muted-foreground">None of this issue&apos;s events are kept any longer.</p>
      ) : event.isError ? (
        <p className="rounded-xl bg-destructive/10 p-4 text-sm text-destructive">
          {event.error instanceof Error ? event.error.message : "Couldn't load that event."}
        </p>
      ) : !event.data ? (
        <Skeleton className="h-96 rounded-2xl" />
      ) : (
        <>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="text-sm text-muted-foreground">
              Event <span className="font-mono">{shorten(event.data.event.event_id)}</span>,{' '}
              <span title={fullDate(event.data.event.received_at)}>{timeAgo(event.data.event.received_at)}</span>
            </p>
            <div className="flex gap-2">
              <Button variant="outline" size="sm" disabled={event.data.older === null} onClick={() => go(event.data.older)}>
                <ChevronLeft className="size-4" aria-hidden /> Older
              </Button>
              <Button variant="outline" size="sm" disabled={event.data.newer === null} onClick={() => go(event.data.newer)}>
                Newer <ChevronRight className="size-4" aria-hidden />
              </Button>
            </div>
          </div>
          <EventDetails hostname={hostname} event={event.data.event} />
        </>
      )}
    </div>
  );
}

function StatusButtons({ hostname, issue }: { hostname: string; issue: IssueDetail }) {
  const setStatus = useSetIssueStatus(hostname);
  const { toast } = useToast();

  const change = async (status: IssueStatus, message: string) => {
    try {
      await setStatus.mutateAsync({ id: issue.id, status });
      toast({ title: message });
    } catch (error) {
      toast({
        title: "Couldn't update the issue",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  if (issue.status !== 'unresolved') {
    return (
      <div className="flex items-center gap-2">
        <Badge variant="secondary">{issue.status === 'resolved' ? 'Resolved' : 'Ignored'}</Badge>
        <Button variant="outline" size="sm" disabled={setStatus.isPending} onClick={() => change('unresolved', 'Issue reopened')}>
          Reopen
        </Button>
      </div>
    );
  }
  return (
    <div className="flex gap-2">
      <Button size="sm" disabled={setStatus.isPending} onClick={() => change('resolved', 'Issue resolved')}>
        Resolve
      </Button>
      <Button variant="outline" size="sm" disabled={setStatus.isPending} onClick={() => change('ignored', 'Issue ignored')}>
        Ignore
      </Button>
    </div>
  );
}

/** The counts, and the last two weeks hour by hour. */
function Overview({ hostname, issue }: { hostname: string; issue: IssueDetail }) {
  const stats = [
    { label: issue.events === 1 ? 'event' : 'events', value: compactNumber(issue.events) },
    { label: issue.visitors === 1 ? 'visitor' : 'visitors', value: compactNumber(issue.visitors) },
    { label: 'first seen', value: timeAgo(issue.first_seen), title: fullDate(issue.first_seen) },
    { label: 'last seen', value: timeAgo(issue.last_seen), title: fullDate(issue.last_seen) },
  ];

  return (
    <div className="space-y-4 rounded-2xl bg-card p-5 surface">
      <dl className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        {stats.map((stat) => (
          <div key={stat.label}>
            <dd className="text-xl font-bold tabular-nums" title={stat.title}>
              {stat.value}
            </dd>
            <dt className="text-xs text-muted-foreground">{stat.label}</dt>
          </div>
        ))}
      </dl>
      <Sparkline values={issue.hours} className="h-16 text-destructive" />
      <p className="text-xs text-muted-foreground">
        The last 14 days.
        {issue.last_release && (
          <>
            {' '}
            Last seen in version{' '}
            <Link to={`/hosts/${hostname}/versions`} className="font-mono text-primary hover:underline">
              {shorten(issue.last_release)}
            </Link>
            .
          </>
        )}
      </p>
    </div>
  );
}

function EventDetails({ hostname, event }: { hostname: string; event: IssueEvent }) {
  const report = event.report;
  const browser = [report.browser, report.browser_version].filter(Boolean).join(' ');
  const os = [report.os, report.os_version].filter(Boolean).join(' ');
  const exceptions = [...report.exceptions].reverse();

  return (
    <div className="space-y-8">
      {report.message && !report.exceptions.length && (
        <Section title="Message">
          <pre className="p-4 font-mono text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">{report.message}</pre>
        </Section>
      )}

      {exceptions.map((exception, index) => (
        <ExceptionView key={index} exception={exception} cause={index > 0} />
      ))}

      {report.breadcrumbs.length > 0 && <Breadcrumbs breadcrumbs={report.breadcrumbs} />}

      <Section title="Context">
        {report.url && <Row label="Page" description={<span className="font-mono break-all">{report.url}</span>} />}
        {browser && <Row label="Browser" description={browser} />}
        {os && <Row label="Operating system" description={os} />}
        {report.device && <Row label="Device" description={report.device} />}
        {report.release && (
          <Row
            label="Version"
            description={
              <Link to={`/hosts/${hostname}/versions`} className="font-mono text-primary hover:underline">
                {report.release}
              </Link>
            }
          />
        )}
        {report.environment && <Row label="Environment" description={report.environment} />}
        {report.sdk && <Row label="Reported by" description={report.sdk} />}
        {Object.entries(report.tags).map(([key, value]) => (
          <Row key={key} label={key} description={value} />
        ))}
      </Section>
    </div>
  );
}

function ExceptionView({ exception, cause }: { exception: ReportedException; cause: boolean }) {
  // The innermost call first, as a stack trace reads.
  const frames = [...exception.frames].reverse();
  const [all, setAll] = useState(false);
  const own = frames.some((frame) => frame.in_app);
  const shown = all || !own ? frames : frames.filter((frame) => frame.in_app);
  const mapped = frames.some((frame) => frame.mapped);

  return (
    <Section
      title={cause ? 'Caused by' : 'Exception'}
      action={
        own &&
        frames.some((frame) => !frame.in_app) && (
          <button type="button" className="text-xs text-primary hover:underline" onClick={() => setAll(!all)}>
            {all ? 'Only the site’s own frames' : `All ${frames.length} frames`}
          </button>
        )
      }
      footer={
        own && !mapped
          ? 'Publish source maps beside your scripts — app.js.map next to app.js — and these frames will show your original code.'
          : undefined
      }
    >
      <div className="space-y-1 p-4">
        <p className="font-mono text-sm font-semibold [overflow-wrap:anywhere]">
          {exception.type}
          {exception.value && <span className="font-normal">: {exception.value}</span>}
        </p>
        {exception.mechanism && (
          <p className="text-xs text-muted-foreground">
            {exception.handled === false ? 'Unhandled, ' : ''}caught by {exception.mechanism}
          </p>
        )}
      </div>
      {shown.map((frame, index) => (
        <FrameView key={index} frame={frame} open={frame.in_app && index === shown.findIndex((f) => f.in_app)} />
      ))}
    </Section>
  );
}

function FrameView({ frame, open }: { frame: StackFrame; open: boolean }) {
  const where = frame.mapped
    ? { file: frame.mapped.source, fn: frame.mapped.function ?? frame.function, line: frame.mapped.lineno, col: frame.mapped.colno }
    : { file: path(frame.abs_path), fn: frame.function, line: frame.lineno, col: frame.colno };

  return (
    <details open={open} className="group border-t">
      <summary
        className={cn(
          'flex cursor-pointer flex-wrap items-baseline gap-x-2 px-4 py-2 font-mono text-xs select-none hover:bg-secondary/50',
          !frame.in_app && 'text-muted-foreground',
        )}
      >
        <span className="[overflow-wrap:anywhere]">{where.file}</span>
        {where.fn && <span className="text-muted-foreground">in</span>}
        {where.fn && <span className="font-semibold">{where.fn}</span>}
        {where.line !== undefined && (
          <span className="text-muted-foreground">
            at line {where.line}
            {where.col !== undefined && `:${where.col}`}
          </span>
        )}
      </summary>
      <div className="space-y-2 px-4 pb-3">
        {frame.context && (
          <pre className="overflow-x-auto rounded-lg bg-secondary/60 py-2 font-mono text-xs leading-relaxed">
            {frame.context.pre.map((line, i) => (
              <ContextLine key={`pre${i}`} number={(where.line ?? 0) - frame.context!.pre.length + i} text={line} />
            ))}
            <ContextLine number={where.line ?? 0} text={frame.context.line} current />
            {frame.context.post.map((line, i) => (
              <ContextLine key={`post${i}`} number={(where.line ?? 0) + 1 + i} text={line} />
            ))}
          </pre>
        )}
        {frame.mapped && (
          <p className="font-mono text-[11px] text-muted-foreground [overflow-wrap:anywhere]">
            {frame.abs_path}:{frame.lineno}
            {frame.colno !== undefined && `:${frame.colno}`}
            {frame.function && ` in ${frame.function}`}
          </p>
        )}
      </div>
    </details>
  );
}

function ContextLine({ number, text, current }: { number: number; text: string; current?: boolean }) {
  return (
    <div className={cn('flex px-3', current && 'bg-destructive/10')}>
      <span className="w-10 shrink-0 pr-3 text-right text-muted-foreground select-none">{number}</span>
      <span className="whitespace-pre">{text}</span>
    </div>
  );
}

function Breadcrumbs({ breadcrumbs }: { breadcrumbs: Breadcrumb[] }) {
  // The latest first, nearest the error.
  const list = [...breadcrumbs].reverse();

  return (
    <Section title="What led up to it">
      {list.map((crumb, index) => (
        <div key={index} className="flex gap-3 border-t px-4 py-2 text-xs first:border-t-0">
          <span className="w-20 shrink-0 font-medium text-muted-foreground">{crumb.category ?? 'event'}</span>
          <span
            className={cn(
              'min-w-0 flex-1 font-mono [overflow-wrap:anywhere]',
              (crumb.level === 'error' || crumb.level === 'fatal') && 'text-destructive',
            )}
          >
            {describeCrumb(crumb)}
          </span>
          {crumb.timestamp !== undefined && (
            <span className="shrink-0 text-muted-foreground tabular-nums" title={fullDate(crumb.timestamp)}>
              {new Date(crumb.timestamp * 1000).toLocaleTimeString()}
            </span>
          )}
        </div>
      ))}
    </Section>
  );
}

/** One line for a breadcrumb: its message, or what its data says. */
function describeCrumb(crumb: Breadcrumb): string {
  const data = crumb.data ?? {};
  if (crumb.category === 'navigation' && data.to !== undefined) return `${data.from ?? ''} → ${data.to}`;
  if ((crumb.category === 'fetch' || crumb.category === 'xhr') && data.url !== undefined) {
    const status = data.status_code !== undefined ? ` [${data.status_code}]` : '';
    return `${data.method ?? 'GET'} ${data.url}${status}${crumb.message ? ` ${crumb.message}` : ''}`;
  }
  if (crumb.message) return crumb.message;
  return Object.entries(data)
    .map(([key, value]) => `${key}=${value}`)
    .join(' ');
}

/** A URL's path, for a frame on someone else's origin as much as the site's own. */
function path(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.pathname === '/' ? url : `${parsed.host}${parsed.pathname}`;
  } catch {
    return url;
  }
}

export default IssuePage;
