import { describe, expect, it } from 'vitest';

import { parseEnvName, parseEnvValue } from './env';

// The same cases as `src/site_env.rs`, so the two rule sets can't drift apart.
describe('parseEnvName', () => {
  it('accepts identifiers, as written', () => {
    expect(parseEnvName('  API_URL ')).toBe('API_URL');
    expect(parseEnvName('_private2')).toBe('_private2');
    expect(parseEnvName('camelCase')).toBe('camelCase');
    expect(parseEnvName('A'.repeat(128))).toBe('A'.repeat(128));
  });

  it('refuses anything else', () => {
    for (const name of ['', '2FAST', 'API-URL', 'A B', 'ÉTÉ', 'A'.repeat(129)]) {
      expect(() => parseEnvName(name)).toThrow();
    }
  });
});

describe('parseEnvValue', () => {
  it('keeps values verbatim, up to the limit', () => {
    expect(parseEnvValue(' spaced ')).toBe(' spaced ');
    expect(parseEnvValue('')).toBe('');
    expect(parseEnvValue('é'.repeat(4096))).toBe('é'.repeat(4096));
    expect(() => parseEnvValue('x'.repeat(4097))).toThrow();
  });
});
