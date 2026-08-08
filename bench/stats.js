// hrtime-based timing + summary stats. All numbers in milliseconds.

export function now() {
  return process.hrtime.bigint();
}

export function ms(fromNs, toNs) {
  return Number(toNs - fromNs) / 1e6;
}

export function summarize(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const n = sorted.length;
  const q = (p) => sorted[Math.min(n - 1, Math.floor(p * n))];
  return {
    n,
    mean: round(sorted.reduce((a, b) => a + b, 0) / n),
    median: round(q(0.5)),
    p95: round(q(0.95)),
    min: round(sorted[0]),
    max: round(sorted[n - 1]),
  };
}

function round(x) {
  return Math.round(x * 1000) / 1000;
}

/** Time `fn` (sync) `iterations` times after `warmup` unrecorded runs. */
export function bench(fn, { warmup, iterations }) {
  for (let i = 0; i < warmup; i++) fn();
  const samples = [];
  for (let i = 0; i < iterations; i++) {
    const t0 = now();
    fn();
    samples.push(ms(t0, now()));
  }
  return summarize(samples);
}
