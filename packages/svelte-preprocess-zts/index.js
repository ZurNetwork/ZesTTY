import { compile } from "@zts/native";

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
 * import ztsPreprocess from "@zts/svelte-preprocess";
 * export default { preprocess: [ztsPreprocess(), vitePreprocess()] };
 * ```
 *
 * @param {{ decorators?: boolean }} [options]
 * @returns {import("svelte/compiler").PreprocessorGroup}
 */
export default function ztsPreprocess(options = {}) {
  return {
    name: "svelte-preprocess-zts",

    script({ content, attributes, filename }) {
      if (attributes.lang !== "zts") return undefined;

      const { code, map } = compile(content, filename ?? "component.svelte", {
        tsx: false,
        decorators: options.decorators,
      });

      return {
        code,
        map,
        attributes: { ...attributes, lang: "ts" },
      };
    },
  };
}
