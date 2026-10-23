// Test base URL guard for browser tests: an explicit target, or the live server only on opt-in.
// The JS half of scripts/lib/test-isolation.sh (a CLI test once wrote into the live fleet, 2026-10-16).

/**
 * The base URL a browser test drives: the named env var when set (an isolated
 * server), else `liveDefault` ONLY when AMUX_TEST_LIVE=1. Without either it
 * exits 2 with the reason, so a run from a lane never lands on the dashboard
 * the owner is using by default.
 */
export function testBase(envName, liveDefault) {
  const explicit = process.env[envName];
  if (explicit) return explicit;
  if (process.env.AMUX_TEST_LIVE === '1') return typeof liveDefault === 'function' ? liveDefault() : liveDefault;
  console.error(`SKIP: set ${envName} to an isolated server, or AMUX_TEST_LIVE=1 to drive the LIVE one (${typeof liveDefault === 'string' ? liveDefault : 'amux url'}).`);
  process.exit(2);
}
