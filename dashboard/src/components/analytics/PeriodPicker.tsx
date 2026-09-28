import { CalendarDays, ChevronDown, ChevronLeft, ChevronRight } from 'lucide-react';
import { useState } from 'react';
import type { DateRange } from 'react-day-picker';

import { Button } from '@/components/ui/button';
import { Calendar } from '@/components/ui/calendar';
import { Checkbox } from '@/components/ui/checkbox';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Popover, PopoverAnchor, PopoverContent } from '@/components/ui/popover';
import { PERIOD_GROUPS, PERIOD_LABELS, canCompare, formatDay, periodLabel, shiftPeriod } from '@/lib/analytics/periods';
import type { ComparisonMode, DashboardState } from '@/lib/analytics/state';

interface PickerProps {
  state: DashboardState;
  onChange(state: DashboardState): void;
  /** Today, in the browser's calendar: the latest day a range may reach. */
  today: string;
}

/**
 * The period dropdown, with arrows that step a day, month or year at a time
 * and a calendar for a range of one's own.
 */
export function PeriodPicker({ state, onChange, today }: PickerProps) {
  const [custom, setCustom] = useState(false);

  const back = shiftPeriod(state, today, -1);
  const forward = shiftPeriod(state, today, 1);

  return (
    <div className="flex items-center gap-1">
      {back !== undefined && (
        <Button
          variant="outline"
          size="icon"
          className="size-9"
          aria-label="Earlier"
          onClick={() => onChange({ ...state, date: back })}
        >
          <ChevronLeft className="size-4" />
        </Button>
      )}
      {(back !== undefined || forward !== undefined) && (
        <Button
          variant="outline"
          size="icon"
          className="size-9"
          aria-label="Later"
          disabled={forward === undefined}
          onClick={() => forward !== undefined && onChange({ ...state, date: forward })}
        >
          <ChevronRight className="size-4" />
        </Button>
      )}

      <Popover open={custom} onOpenChange={setCustom}>
        <DropdownMenu>
          <PopoverAnchor asChild>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" className="min-w-40 justify-between">
                <span className="flex items-center gap-2">
                  <CalendarDays className="size-4" />
                  {periodLabel(state)}
                </span>
                <ChevronDown className="size-4 opacity-60" />
              </Button>
            </DropdownMenuTrigger>
          </PopoverAnchor>
          <DropdownMenuContent align="start" className="w-52">
            {PERIOD_GROUPS.map((group, index) => (
              <div key={group[0]}>
                {index > 0 && <DropdownMenuSeparator />}
                {group.map((period) => (
                  <DropdownMenuItem
                    key={period}
                    onSelect={() =>
                      onChange({
                        ...state,
                        period,
                        date: null,
                        from: null,
                        to: null,
                        comparison: period === 'realtime' || period === 'all' ? 'off' : state.comparison,
                      })
                    }
                  >
                    {PERIOD_LABELS[period]}
                  </DropdownMenuItem>
                ))}
              </div>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => setCustom(true)}>Custom range…</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>

        <PopoverContent align="start" className="w-auto p-0">
          <RangeCalendar
            today={today}
            initial={state.period === 'custom' ? [state.from, state.to] : [null, null]}
            onPick={(from, to) => {
              setCustom(false);
              onChange({ ...state, period: 'custom', date: null, from, to });
            }}
          />
        </PopoverContent>
      </Popover>
    </div>
  );
}

const COMPARISON_LABELS: Record<ComparisonMode, string> = {
  off: 'Disable comparison',
  previous_period: 'Previous period',
  year_over_year: 'Year over year',
  custom: 'Custom period…',
};

/** Whether, and against what, the period is compared. */
export function ComparisonPicker({ state, onChange, today }: PickerProps) {
  const [custom, setCustom] = useState(false);

  if (!canCompare(state)) return null;

  const label =
    state.comparison === 'off'
      ? 'Compare'
      : state.comparison === 'custom' && state.compareFrom && state.compareTo
        ? `vs. ${formatDay(state.compareFrom)} – ${formatDay(state.compareTo)}`
        : `vs. ${COMPARISON_LABELS[state.comparison].toLowerCase()}`;

  return (
    <Popover open={custom} onOpenChange={setCustom}>
      <DropdownMenu>
        <PopoverAnchor asChild>
          <DropdownMenuTrigger asChild>
            <Button variant="outline" className="justify-between gap-2">
              {label}
              <ChevronDown className="size-4 opacity-60" />
            </Button>
          </DropdownMenuTrigger>
        </PopoverAnchor>
        <DropdownMenuContent align="start" className="w-60">
          <DropdownMenuRadioGroup
            value={state.comparison}
            onValueChange={(value) => {
              if (value === 'custom') return setCustom(true);
              onChange({ ...state, comparison: value as ComparisonMode });
            }}
          >
            {(Object.keys(COMPARISON_LABELS) as ComparisonMode[]).map((mode) => (
              <DropdownMenuRadioItem key={mode} value={mode}>
                {COMPARISON_LABELS[mode]}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          {state.comparison !== 'off' && state.comparison !== 'custom' && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onSelect={(event) => {
                  event.preventDefault();
                  onChange({ ...state, matchDayOfWeek: !state.matchDayOfWeek });
                }}
              >
                <Checkbox checked={state.matchDayOfWeek} className="mr-2" />
                Match day of the week
              </DropdownMenuItem>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>

      <PopoverContent align="start" className="w-auto p-0">
        <RangeCalendar
          today={today}
          initial={[state.compareFrom, state.compareTo]}
          onPick={(from, to) => {
            setCustom(false);
            onChange({ ...state, comparison: 'custom', compareFrom: from, compareTo: to });
          }}
        />
      </PopoverContent>
    </Popover>
  );
}

/** Two clicks for a range, the second closing it; one date twice is a single day. */
function RangeCalendar({
  today,
  initial,
  onPick,
}: {
  today: string;
  initial: [string | null, string | null];
  onPick(from: string, to: string): void;
}) {
  const [range, setRange] = useState<DateRange | undefined>(() =>
    initial[0] && initial[1] ? { from: toDate(initial[0]), to: toDate(initial[1]) } : undefined,
  );

  return (
    <div className="space-y-2 p-2">
      <Calendar
        mode="range"
        numberOfMonths={2}
        selected={range}
        onSelect={setRange}
        disabled={{ after: toDate(today) }}
        defaultMonth={range?.from ?? toDate(today)}
      />
      <div className="flex justify-end px-2 pb-1">
        <Button
          size="sm"
          disabled={!range?.from}
          onClick={() => range?.from && onPick(fromDate(range.from), fromDate(range.to ?? range.from))}
        >
          Apply
        </Button>
      </div>
    </div>
  );
}

function toDate(day: string): Date {
  const [year, month, date] = day.split('-').map(Number);
  return new Date(year, month - 1, date);
}

function fromDate(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}
