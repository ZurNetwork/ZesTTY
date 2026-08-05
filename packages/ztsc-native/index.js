import { createRequire } from "node:module";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const dir = dirname(fileURLToPath(import.meta.url));

// Single prebuilt target for now. Widen this table when more platforms
// get builds (see @napi-rs/cli for the full matrix approach).
const CANDIDATES = ["ztsc.linux-x64-gnu.node"];

function load() {
  const errors = [];
  for (const name of CANDIDATES) {
    try {
      return require(join(dir, name));
    } catch (err) {
      errors.push(`  ${name}: ${err.message}`);
    }
  }
  throw new Error(
    `@zts/native: no loadable native module for ${process.platform}-${process.arch}.\n` +
      `Run \`npm run build:native\` at the repo root.\n` +
      errors.join("\n"),
  );
}

const binding = load();

/**
 * Compile zts source text to TypeScript.
 *
 * @param {string} source
 * @param {string} filename
 * @param {{ tsx?: boolean, decorators?: boolean, inlineSourcesContent?: boolean }} [options]
 * @returns {{ code: string, map: string }}
 * @throws {Error} with rendered zts diagnostics as the message
 */
export function compile(source, filename, options) {
  return binding.compile(source, filename, options);
}
