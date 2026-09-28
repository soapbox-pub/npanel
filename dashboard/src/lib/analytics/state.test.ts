import { describe, expect, test } from 'vitest';

import { shiftPeriod, validIntervals } from './periods';
import { statsQuery } from './query';
import { DEFAULT_STATE, apiFilters, parseState, serializeState, withFilter } from './state';

describe('dashboard state in the URL', () => {
  test('round-trips filters, labels and a comparison', () => {
    const state = withFilter(
      {
        ...DEFAULT_STATE,
        period: 'custom',
        from: '2026-09-01',
        to: '2026-09-14',
        comparison: 'year_over_year',
        matchDayOfWeek: false,
        filters: [{ operator: 'contains', key: 'page', clauses: ['/blog, and more'] }],
      },
      { operator: 'is', key: 'country', clauses: ['DE', 'FR'] },
      { DE: 'Germany', FR: 'France' },
    );

    expect(parseState(serializeState(state))).toEqual(state);
  });

  test('leaves the defaults out, and ignores what does not parse', () => {
    expect(serializeState(DEFAULT_STATE).toString()).toBe('');

    const parsed = parseState(new URLSearchParams('period=forever&f=is,nonsense,x&f=is,page&comparison=maybe'));
    expect(parsed).toEqual(DEFAULT_STATE);
  });

  test('a custom range without both ends falls back to the default period', () => {
    expect(parseState(new URLSearchParams('period=custom&from=2026-09-01')).period).toBe('28d');
  });

  test('setting a filter replaces the one on the same key', () => {
    const once = withFilter(DEFAULT_STATE, { operator: 'is', key: 'source', clauses: ['Google'] });
    const twice = withFilter(once, { operator: 'is_not', key: 'source', clauses: ['Bing'] });

    expect(apiFilters(twice.filters)).toEqual([['is_not', 'visit:source', ['Bing']]]);
  });
});

describe('periods', () => {
  test('intervals follow the period, and a custom range its length', () => {
    expect(validIntervals({ ...DEFAULT_STATE, period: 'realtime' })).toEqual(['minute']);
    expect(validIntervals({ ...DEFAULT_STATE, period: '12mo' })[0]).toBe('month');
    expect(validIntervals({ ...DEFAULT_STATE, period: 'custom', from: '2026-01-01', to: '2026-01-10' })[0]).toBe('day');
    expect(validIntervals({ ...DEFAULT_STATE, period: 'custom', from: '2025-01-01', to: '2026-01-10' })[0]).toBe('month');
  });

  test('calendar periods step, but never into the future', () => {
    const today = '2026-09-28';
    expect(shiftPeriod({ ...DEFAULT_STATE, period: 'day' }, today, -1)).toBe('2026-09-27');
    expect(shiftPeriod({ ...DEFAULT_STATE, period: 'day' }, today, 1)).toBeUndefined();
    expect(shiftPeriod({ ...DEFAULT_STATE, period: 'day', date: '2026-09-27' }, today, 1)).toBeNull();
    expect(shiftPeriod({ ...DEFAULT_STATE, period: 'month' }, today, -1)).toBe('2026-08-01');
    expect(shiftPeriod({ ...DEFAULT_STATE, period: '7d' }, today, -1)).toBeUndefined();
  });
});

describe('queries', () => {
  test('are built the way Plausible builds them', () => {
    const state = { ...DEFAULT_STATE, comparison: 'previous_period' as const };
    const body = statsQuery(withFilter(state, { operator: 'is', key: 'screen', clauses: ['Mobile'] }), {
      metrics: ['visitors'],
      dimensions: ['time:day'],
      timeLabels: true,
    });

    expect(body).toMatchObject({
      date_range: '28d',
      relative_date: null,
      filters: [['is', 'visit:device', ['Mobile']]],
      include: { compare: 'previous_period', compare_match_day_of_week: true, time_labels: true },
    });
  });

  test('never compare the live view', () => {
    const state = { ...DEFAULT_STATE, period: 'realtime' as const, comparison: 'previous_period' as const };
    expect(statsQuery(state, { metrics: ['visitors'] }).include.compare).toBeNull();
  });
});
