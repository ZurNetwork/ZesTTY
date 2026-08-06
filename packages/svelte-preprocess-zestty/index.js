import { compile } from "@zestty/native";

/**
 * Svelte preprocessor for `<script lang="zts">`.
 *
 * Compiles the script content zts → TypeScript and rewrites the `lang`
 * attribute to `"ts"`, so the rest of the chain (`vitePreprocess`,
 * svelte-check) treats the output exactly like a hand-written
 * `<script lang="ts">`. Chain it BEFORE vitePreprocess:
 *
 * ```js
 * // svelte.config.js
 * import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";
 * import ztsPreprocess from "@zestty/svelte-preprocess";
 * export default { preprocess: [ztsPreprocess(), vitePreprocess()] };
 * ```
 *
 * @param {{ decorators?: boolean, inlinePreamble?: boolean }} [options]
 * @returns {import("svelte/compiler").PreprocessorGroup}
 */
export default function ztsPreprocess(options = {}) {
  return {
    name: "svelte-preprocess-zestty",

    script({ content, attributes, filename }) {
      if (attributes.lang !== "zts") return undefined;

      const { code, map } = compile(content, filename ?? "component.svelte", {
        tsx: false,
        decorators: options.decorators,
        // Default (issue #47): import the one shared __ztsAbsurd from
        // @zestty/core; `inlinePreamble: true` restores the per-script
        // helper for projects without the dependency.
        ...(options.inlinePreamble ? { preambleImport: false } : {}),
      });

      return {
        code,
        map,
        attributes: { ...attributes, lang: "ts" },
      };
    },
  };
}
