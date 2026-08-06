#!/usr/bin/env node
import { ztsCheck } from "./lib.js";

const args = process.argv.slice(2);
const keep = args.includes("--keep");
const svelte = !args.includes("--no-svelte");
const root = args.find((a) => !a.startsWith("--")) ?? process.cwd();

if (args.includes("--help") || args.includes("-h")) {
  console.error("usage: zts-check [root] [--keep]");
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

process.exit(ztsCheck(root, { keep, svelte }));
