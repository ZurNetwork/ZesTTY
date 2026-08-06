#!/usr/bin/env node
import { ztsCheck } from "./lib.js";

const args = process.argv.slice(2);
const keep = args.includes("--keep");
const twins = args.includes("--twins");
const watchMode = args.includes("--watch");
const inlinePreamble = args.includes("--inline-preamble");
const svelte = !args.includes("--no-svelte");
const execIdx = args.indexOf("--exec");
const execCmd = execIdx !== -1 ? args[execIdx + 1] : null;
const positional = args.filter(
  (a, i) => !a.startsWith("--") && !(execIdx !== -1 && i === execIdx + 1),
);
const root = positional[0] ?? process.cwd();

if (args.includes("--help") || args.includes("-h")) {
  console.error(
    "usage: zts-check [root] [--keep] [--no-svelte] [--twins] [--inline-preamble] [--watch [--exec <cmd>]]",
  );
  console.error("");
  console.error(
    'Compiles .zts/.ztsx modules and <script lang="zts"> blocks into',
  );
  console.error(
    "shadow twins, runs tsc --noEmit, and remaps diagnostics back to the",
  );
  console.error(
    "original zts sources. --keep leaves the generated twins in place.",
  );
  process.exit(2);
}

if (watchMode && !twins) {
  console.error("zts-check: --watch requires --twins");
  process.exit(2);
}
if (execCmd === null && execIdx !== -1) {
  console.error("zts-check: --exec requires a command argument");
  process.exit(2);
}
if (twins) {
  const { generateTwins, watchTwins } = await import("./lib.js");
  if (watchMode) {
    const handle = watchTwins(root, {
      exec: execCmd,
      preambleImport: !inlinePreamble,
    });
    if (!handle) process.exit(2);
    process.on("SIGINT", () => {
      handle.close();
      process.exit(0);
    });
    // Keep the process alive; the watcher drives everything.
  } else {
    process.exit(generateTwins(root, { preambleImport: !inlinePreamble }));
  }
} else {
  process.exit(ztsCheck(root, { keep, svelte }));
}
