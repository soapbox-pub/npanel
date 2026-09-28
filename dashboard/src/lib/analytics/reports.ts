import type { Dimension, FilterOperator, Metric } from '@shared/analytics-query';

import type { FilterKey } from './state';

export type ApiFilter =
  | [FilterOperator, Dimension, (string | number)[]]
  | [FilterOperator, Dimension, (string | number)[], { case_sensitive: boolean }];

/**
 * One list in a report card: which breakdown it is, and what the details
 * dialog shows for it. The same lists and metrics as Plausible's dashboard
 * (`reports-config.ts`), so a card here reads like the card there.
 */
export interface ReportList {
  id: string;
  /** The tab's name. */
  tab: string;
  /** The column heading, and the details dialog's title. */
  title: string;
  dimensions: Dimension[];
  /** Which dimension is shown as the row's name. */
  labelIndex: number;
  /** Which dimension a click filters by, and the filter it sets. */
  valueIndex: number;
  filterKey: FilterKey;
  /** Which dimension holds the country a flag is drawn for. */
  countryIndex?: number;
  metrics: Metric[];
  detailsMetrics: Metric[];
  /** Filters that keep blank values out of a list where a blank means nothing. */
  alwaysOn: ApiFilter[];
  /** Rows are pages of the site, and can be opened. */
  pages?: boolean;
}

const INDEX: Metric[] = ['visitors', 'percentage'];
const VISIT_DETAILS: Metric[] = ['visitors', 'percentage', 'bounce_rate', 'visit_duration'];

function simple(
  id: string,
  tab: string,
  title: string,
  dimension: Dimension,
  filterKey: FilterKey,
  alwaysOn: ApiFilter[] = [],
): ReportList {
  return {
    id,
    tab,
    title,
    dimensions: [dimension],
    labelIndex: 0,
    valueIndex: 0,
    filterKey,
    metrics: INDEX,
    detailsMetrics: VISIT_DETAILS,
    alwaysOn,
  };
}

function utm(key: 'utm_medium' | 'utm_source' | 'utm_campaign' | 'utm_content' | 'utm_term', tab: string, title: string) {
  const dimension = `visit:${key}` as Dimension;
  return simple(key, tab, title, dimension, key, [['is_not', dimension, ['']]]);
}

export const REPORTS: Record<string, ReportList> = {
  channels: simple('channels', 'Channels', 'Channel', 'visit:channel', 'channel'),
  sources: simple('sources', 'Sources', 'Source', 'visit:source', 'source'),
  referrers: simple('referrers', 'Referrers', 'Referrer', 'visit:referrer', 'referrer'),
  utm_medium: utm('utm_medium', 'UTM Medium', 'UTM Medium'),
  utm_source: utm('utm_source', 'UTM Source', 'UTM Source'),
  utm_campaign: utm('utm_campaign', 'UTM Campaign', 'UTM Campaign'),
  utm_content: utm('utm_content', 'UTM Content', 'UTM Content'),
  utm_term: utm('utm_term', 'UTM Term', 'UTM Term'),

  pages: {
    ...simple('pages', 'Top pages', 'Page', 'event:page', 'page'),
    detailsMetrics: ['visitors', 'percentage', 'pageviews', 'bounce_rate', 'time_on_page', 'scroll_depth'],
    pages: true,
  },
  entry_pages: {
    ...simple('entry_pages', 'Entry pages', 'Entry page', 'visit:entry_page', 'entry_page', [
      ['is_not', 'visit:entry_page', ['']],
    ]),
    detailsMetrics: ['visitors', 'percentage', 'visits', 'bounce_rate', 'visit_duration'],
    pages: true,
  },
  exit_pages: {
    ...simple('exit_pages', 'Exit pages', 'Exit page', 'visit:exit_page', 'exit_page', [
      ['is_not', 'visit:exit_page', ['']],
    ]),
    detailsMetrics: ['visitors', 'percentage', 'visits', 'exit_rate'],
    pages: true,
  },

  countries: {
    ...simple('countries', 'Countries', 'Country', 'visit:country_name', 'country', [
      ['is_not', 'visit:country', ['\u0000\u0000', 'ZZ']],
    ]),
    dimensions: ['visit:country_name', 'visit:country'],
    valueIndex: 1,
    countryIndex: 1,
  },
  regions: {
    ...simple('regions', 'Regions', 'Region', 'visit:region_name', 'region', [['is_not', 'visit:region', ['']]]),
    dimensions: ['visit:region_name', 'visit:region', 'visit:country'],
    valueIndex: 1,
    countryIndex: 2,
  },
  cities: {
    ...simple('cities', 'Cities', 'City', 'visit:city_name', 'city', [['is_not', 'visit:city', [0]]]),
    dimensions: ['visit:city_name', 'visit:city', 'visit:country'],
    valueIndex: 1,
    countryIndex: 2,
  },

  browsers: simple('browsers', 'Browsers', 'Browser', 'visit:browser', 'browser'),
  browser_versions: {
    ...simple('browser_versions', 'Browser versions', 'Version', 'visit:browser_version', 'browser_version'),
    dimensions: ['visit:browser_version', 'visit:browser'],
  },
  operating_systems: simple('operating_systems', 'Operating systems', 'Operating system', 'visit:os', 'os'),
  operating_system_versions: {
    ...simple('operating_system_versions', 'OS versions', 'Version', 'visit:os_version', 'os_version'),
    dimensions: ['visit:os_version', 'visit:os'],
  },
  screen_sizes: simple('screen_sizes', 'Devices', 'Device', 'visit:device', 'screen'),
};

/** Location filters' values are codes, and their pills want the names. */
export const LABELLED_FILTERS: readonly FilterKey[] = ['country', 'region', 'city'];

/** What each filter key is called, in the filter dialog and on its pill. */
export const FILTER_LABELS: Record<FilterKey, string> = {
  page: 'Page',
  entry_page: 'Entry page',
  exit_page: 'Exit page',
  source: 'Source',
  channel: 'Channel',
  referrer: 'Referrer URL',
  utm_medium: 'UTM Medium',
  utm_source: 'UTM Source',
  utm_campaign: 'UTM Campaign',
  utm_content: 'UTM Content',
  utm_term: 'UTM Term',
  country: 'Country',
  region: 'Region',
  city: 'City',
  screen: 'Device',
  browser: 'Browser',
  browser_version: 'Browser version',
  os: 'Operating system',
  os_version: 'OS version',
};

/** The filter dialog's groups. */
export const FILTER_GROUPS: { title: string; keys: FilterKey[] }[] = [
  { title: 'Pages', keys: ['page', 'entry_page', 'exit_page'] },
  { title: 'Acquisition', keys: ['source', 'channel', 'referrer'] },
  { title: 'UTM tags', keys: ['utm_medium', 'utm_source', 'utm_campaign', 'utm_content', 'utm_term'] },
  { title: 'Location', keys: ['country', 'region', 'city'] },
  { title: 'Devices', keys: ['screen', 'browser', 'browser_version', 'os', 'os_version'] },
];
