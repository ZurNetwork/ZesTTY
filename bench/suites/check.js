// zts-check wall-clock over a synthetic project: full CI-gate cost
// (compile every module + tsc --noEmit over the shadow tree + remap).
// Runs the bin exactly as CI would, from a cold process each time.

import { spawn } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { now, ms, summarize } from "../stats.js";
import { generateProject } from "../gen.js";

export async function run(
  { repoRoot, workDir, files, sectionsPerFile, runs },
  log,
) {
  const proj = join(workDir, "check", "proj");
  rmSync(proj, { recursive: true, force: true });
  mkdirSync(proj, { recursive: true });
  const modules = generateProject(files, sectionsPerFile);
  for (const [name, source] of Object.entries(modules)) {
    writeFileSync(join(proj, name), source);
  }
  // The shadow tree gets its own tsconfig from zts-check; this one keeps
  // any direct tsc invocation over the project sane. typescript resolves
  // from the repo root node_modules (proj lives under bench/.work).
  writeFileSync(
    join(proj, "tsconfig.json"),
    JSON.stringify(
      { compilerOptions: { strict: true, noEmit: true, skipLibCheck: true } },
      null,
      2,
    ),
  );
  log(`check: project ${files} modules × ${sectionsPerFile} sections`);

  const bin = join(repoRoot, "packages/check/bin.js");
  const samples = [];
  for (let i = 0; i < runs; i++) {
    const t0 = now();
    const code = await new Promise((resolve, reject) => {
      const p = spawn(process.execPath, [bin, proj, "--no-svelte"], {
        cwd: proj,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let err = "";
      p.stderr.on("data", (d) => (err += d));
      p.stdout.resume();
      p.on("error", reject);
      p.on("exit", (c) => resolve({ c, err }));
    });
    samples.push(ms(t0, now()));
    if (code.c !== 0) {
      throw new Error(
        `zts-check exited ${code.c} on the synthetic project:\n${code.err}`,
      );
    }
  }
  return { "check.wallclock": summarize(samples) };
}
