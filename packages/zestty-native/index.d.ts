export interface CompileOptions {
  /** Parse JSX (for .ztsx). Default false. */
  tsx?: boolean;
  /** Parse decorators. Default true. */
  decorators?: boolean;
  /** Embed original source text in the sourcemap. Default true. */
  inlineSourcesContent?: boolean;
}

export interface CompileResult {
  /** Generated TypeScript (no sourceMappingURL comment). */
  code: string;
  /** Sourcemap JSON mapping the TS back to the zts source. */
  map: string;
}

/**
 * Compile zts source text to TypeScript.
 * Throws an Error whose message is the rendered zts diagnostics.
 */
export function compile(
  source: string,
  filename: string,
  options?: CompileOptions,
): CompileResult;

/** Prettier-shaped zts-fmt option subset (issue #70). Every field
 * defaults to the `zts-fmt.json` discovered upward from the formatted
 * file, then printWidth 80 / spaces / double quotes / imports kept in
 * source order. */
export interface FormatOptions {
  /** prettier printWidth; default 80. */
  printWidth?: number;
  /** prettier useTabs; default false. */
  useTabs?: boolean;
  /** prettier singleQuote; default false. */
  singleQuote?: boolean;
  /** Opt in to dprint's import/export sorting; default false —
   * canonical emit never reorders module declarations. */
  sortImports?: boolean;
}

/**
 * Format zts source (Phase 7: zts-fmt via the dprint fork). Returns
 * null when the input is already formatted.
 * Throws an Error when the source does not parse or the discovered
 * zts-fmt.json is invalid (unknown keys are errors, not ignored).
 */
export function format(
  source: string,
  filename: string,
  options?: FormatOptions,
): string | null;
