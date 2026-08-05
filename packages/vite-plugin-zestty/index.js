import { compile } from "@zestty/native";
import { transformWithEsbuild } from "vite";

const ZTS_RE = /\.ztsx?$/;

/**
 * Vite plugin for `.zts` / `.ztsx` modules.
 *
 * Stage 1 (native zestty): zts → TypeScript + sourcemap.
 * Stage 2 (vite's own esbuild): TypeScript → JS, with the stage-1 map fed
 * in as `inMap` so the composed map points all the way back to the `.zts`
 * source — breakpoints set in `.zts` bind in devtools.
 *
 * @param {{ decorators?: boolean }} [options]
 * @returns {import("vite").Plugin}
 */
export default function zts(options = {}) {
  let isProduction = false;

  return {
    name: "vite-plugin-zestty",
    // Run before vite's own transforms so esbuild never sees raw zts.
    enforce: "pre",

    configResolved(config) {
      isProduction = config.isProduction;
    },

    async transform(code, id) {
      // Strip vite's query (`?import`, `?raw`) and any fragment.
      const [file] = id.split(/[?#]/, 1);
      if (!ZTS_RE.test(file)) return null;

      let stage1;
      try {
        stage1 = compile(code, file, {
          tsx: file.endsWith(".ztsx"),
          decorators: options.decorators,
          inlineSourcesContent: !isProduction,
        });
      } catch (err) {
        // Rendered zts diagnostics → vite error overlay. Rollup's
        // this.error throws, but `return` guards hosts where it doesn't —
        // falling through would bury the diagnostic under a TypeError.
        return this.error(`zts: ${err.message}`);
      }

      const result = await transformWithEsbuild(
        stage1.code,
        // Trailing .ts tells esbuild which loader to use while keeping the
        // original id visible in logs.
        `${file}.ts`,
        {
          loader: file.endsWith(".ztsx") ? "tsx" : "ts",
          sourcemap: true,
        },
        JSON.parse(stage1.map),
      );

      return { code: result.code, map: result.map };
    },
  };
}
