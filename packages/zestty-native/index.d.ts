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

/**
 * Format zts source (Phase 7: zts-fmt via the dprint fork). Returns
 * null when the input is already formatted.
 * Throws an Error when the source does not parse.
 */
export function format(source: string, filename: string): string | null;
