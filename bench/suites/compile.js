// Compile-time suite through @zestty/native — the real consumer path
// (vite plugin, svelte preprocessor, zts-check all sit on this binding),
// so it feels the per-call thread spawn the toolchain round targets.

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import * as native from "@zestty/native";
import { bench } from "../stats.js";
import { generateModule } from "../gen.js";

export function run({ repoRoot, sections, warmup, iterations }, log) {
  const fixtureDir = join(repoRoot, "tests/fixtures");
  // Top-level fixtures only: fixtures/errors/ holds deliberate zts
  // diagnostics; a handful of top-level ones fail at compile() too
  // (they exist to fail tsc or the semantic pass) — pre-filter so the
  // timed corpus is stable and green.
  const corpus = [];
  let skipped = 0;
  for (const name of readdirSync(fixtureDir).sort()) {
    if (!name.endsWith(".zts") && !name.endsWith(".ztsx")) continue;
    const source = readFileSync(join(fixtureDir, name), "utf8");
    try {
      native.compile(source, name, { tsx: name.endsWith(".ztsx") });
      corpus.push({ name, source });
    } catch {
      skipped++;
    }
  }
  log(
    `compile: fixture corpus ${corpus.length} files (${skipped} non-compiling skipped)`,
  );

  const synthetic = generateModule(sections);
  log(
    `compile: synthetic module ${synthetic.split("\n").length} lines (${sections} sections)`,
  );
  const small = readFileSync(join(fixtureDir, "enum_basic.zts"), "utf8");

  const results = {
    "compile.fixtures_corpus": bench(
      () => {
        for (const { name, source } of corpus) {
          native.compile(source, name, { tsx: name.endsWith(".ztsx") });
        }
      },
      { warmup, iterations },
    ),
    "compile.synthetic_large": bench(
      () => native.compile(synthetic, "synthetic.zts"),
      { warmup, iterations },
    ),
    // A tiny file is almost pure per-call overhead (thread spawn etc.) —
    // the number the reusable-worker change must move.
    "compile.small_file": bench(() => native.compile(small, "enum_basic.zts"), {
      warmup,
      iterations: iterations * 5,
    }),
  };

  if (typeof native.format === "function") {
    results["format.synthetic_large"] = bench(
      () => native.format(synthetic, "synthetic.zts"),
      { warmup, iterations },
    );
  }

  return results;
}
