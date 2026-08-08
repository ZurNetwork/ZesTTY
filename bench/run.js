#!/usr/bin/env node
// Orchestrator: runs the suites, prints a table, writes a results JSON,
// and diffs against a baseline. The slate rule this exists to enforce:
// no optimization lands without a before/after number from here.
//
//   node bench/run.js [--suite compile,ls,check] [--smoke]
//                     [--out file.json] [--baseline file.json]

import { execSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { cpus } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const benchDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(benchDir);
const workDir = join(benchDir, ".work");

const args = process.argv.slice(2);
const flag = (name) => {
  const i = args.indexOf(name);
  return i !== -1 ? (args[i + 1] ?? null) : null;
};
const smoke = args.includes("--smoke");
const suites = (flag("--suite") ?? "compile,ls,check").split(",");
const baselinePath = flag("--baseline");

// Smoke mode exists for the test suite: prove the harness end-to-end in
// seconds. Its numbers are meaningless — never record them.
const config = smoke
  ? {
      sections: 4,
      warmup: 1,
      iterations: 2,
      keystrokes: 5,
      completions: 3,
      files: 3,
      sectionsPerFile: 2,
      runs: 1,
    }
  : {
      sections: 60,
      warmup: 3,
      iterations: 10,
      keystrokes: 50,
      completions: 25,
      files: 12,
      sectionsPerFile: 6,
      runs: 3,
    };

const log = (line) => console.error(`  ${line}`);
const results = {};

for (const suite of suites) {
  console.error(`▸ ${suite}`);
  const mod = await import(`./suites/${suite}.js`);
  Object.assign(results, await mod.run({ repoRoot, workDir, ...config }, log));
}

const sha = execSync("git rev-parse --short HEAD", { cwd: repoRoot })
  .toString()
  .trim();
const out = {
  meta: {
    sha,
    smoke,
    date: new Date().toISOString(),
    node: process.version,
    cpu: cpus()[0]?.model ?? "unknown",
    config,
  },
  results,
};

// ---- report ----
const keys = Object.keys(results);
const pad = Math.max(...keys.map((k) => k.length));
console.log(
  `\n${"benchmark".padEnd(pad)}  ${"mean".padStart(9)}  ${"median".padStart(9)}  ${"p95".padStart(9)}  n`,
);
for (const k of keys) {
  const r = results[k];
  console.log(
    `${k.padEnd(pad)}  ${fmt(r.mean)}  ${fmt(r.median)}  ${fmt(r.p95)}  ${r.n}`,
  );
}

if (baselinePath) {
  const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
  console.log(`\nvs baseline ${baseline.meta.sha} (${baseline.meta.date})`);
  for (const k of keys) {
    const b = baseline.results[k];
    if (!b) continue;
    const delta = ((results[k].mean - b.mean) / b.mean) * 100;
    const sign = delta > 0 ? "+" : "";
    console.log(
      `${k.padEnd(pad)}  ${fmt(b.mean)} → ${fmt(results[k].mean)}  (${sign}${delta.toFixed(1)}%)`,
    );
  }
}

const outPath =
  flag("--out") ??
  join(
    benchDir,
    "results",
    `${out.meta.date.replaceAll(":", "-")}-${sha}.json`,
  );
mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, JSON.stringify(out, null, 2) + "\n");
console.log(`\nresults → ${outPath}`);

function fmt(x) {
  return `${x.toFixed(2)}ms`.padStart(9);
}
