/**
 * @zestty/core — the ZesTTY (zts) runtime library.
 *
 * `Result<T, E>` is a plain tagged union using the same `kind` discriminant
 * as every other zts construct, so it composes with `match`:
 *
 * ```zts
 * const msg = match (divide(a, b)) {
 *   Ok { value } => `quotient: ${value}`,
 *   Err { error } => `failed: ${error}`,
 * };
 * ```
 *
 * Nothing here is compiler magic — this is a library feature, shipped as
 * data + functions. What survives to runtime was always just tagged objects.
 */

export type Ok<T> = { readonly kind: "Ok"; readonly value: T };
export type Err<E> = { readonly kind: "Err"; readonly error: E };

export type Result<T, E> = Ok<T> | Err<E>;

/** Wrap a success value. */
export function Ok<T>(value: T): Ok<T> {
  return { kind: "Ok", value };
}

/** Wrap an error value. */
export function Err<E>(error: E): Err<E> {
  return { kind: "Err", error };
}

/**
 * Transform the success value, passing errors through untouched.
 *
 * ```ts
 * map(Ok(2), (n) => n * 2)   // Ok(4)
 * map(Err("boom"), (n) => n) // Err("boom")
 * ```
 */
export function map<T, U, E>(
  r: Result<T, E>,
  f: (value: T) => U,
): Result<U, E> {
  return r.kind === "Ok" ? Ok(f(r.value)) : r;
}

/**
 * Transform the error value, passing successes through untouched.
 */
export function map_err<T, E, F>(
  r: Result<T, E>,
  f: (error: E) => F,
): Result<T, F> {
  return r.kind === "Err" ? Err(f(r.error)) : r;
}

/** Type guard: narrows a Result to its Ok side. */
export function is_ok<T, E>(r: Result<T, E>): r is Ok<T> {
  return r.kind === "Ok";
}

/** Type guard: narrows a Result to its Err side. */
export function is_err<T, E>(r: Result<T, E>): r is Err<E> {
  return r.kind === "Err";
}

/**
 * Extract the success value or throw the error value.
 *
 * The escape hatch back into exception-land; prefer `match`.
 */
export function unwrap<T, E>(r: Result<T, E>): T {
  if (r.kind === "Ok") {
    return r.value;
  }
  throw r.error;
}

/** Extract the success value, or `fallback` on Err. */
export function unwrap_or<T, E>(r: Result<T, E>, fallback: T): T {
  return r.kind === "Ok" ? r.value : fallback;
}

/**
 * Chain a Result-returning computation onto a success (Rust's `and_then`,
 * a.k.a. flatMap), passing errors through untouched.
 *
 * ```ts
 * and_then(parsePort(raw), (p) => checkRange(p))  // Result<Port, string>
 * ```
 */
export function and_then<T, U, E>(
  r: Result<T, E>,
  f: (value: T) => Result<U, E>,
): Result<U, E> {
  return r.kind === "Ok" ? f(r.value) : r;
}

/**
 * Rust-style left-to-right chaining over a Result:
 *
 * ```ts
 * const out = ResultPipe(parsePort(raw))
 *   .map((p) => p + 1)
 *   .map_err((e) => `boot failed: ${e}`)
 *   .done();
 * ```
 *
 * The pipe is EPHEMERAL sugar over the free functions: what goes in and
 * what `done()` returns is the same plain `{ kind, ... }` tagged object.
 * Nothing serializable ever carries a prototype — never store, return, or
 * send the pipe itself; finish every chain with `done()` (or one of the
 * unwrap terminals).
 */
export type ResultPipe<T, E> = {
  /** Transform the success value ([map]). */
  map<U>(f: (value: T) => U): ResultPipe<U, E>;
  /** Transform the error value ([map_err]). */
  map_err<F>(f: (error: E) => F): ResultPipe<T, F>;
  /** Chain a Result-returning computation ([and_then]). */
  and_then<U>(f: (value: T) => Result<U, E>): ResultPipe<U, E>;
  /** Finish the chain, returning the plain Result. */
  done(): Result<T, E>;
  /** Finish and extract, throwing the error value ([unwrap]). */
  unwrap(): T;
  /** Finish and extract, or `fallback` on Err ([unwrap_or]). */
  unwrap_or(fallback: T): T;
};

export function ResultPipe<T, E>(r: Result<T, E>): ResultPipe<T, E> {
  return {
    map: (f) => ResultPipe(map(r, f)),
    map_err: (f) => ResultPipe(map_err(r, f)),
    and_then: (f) => ResultPipe(and_then(r, f)),
    done: () => r,
    unwrap: () => unwrap(r),
    unwrap_or: (fallback) => unwrap_or(r, fallback),
  };
}

/**
 * Narrows a possibly-empty array to zts's non-empty shape (`T[+]`,
 * which lowers to `[T, ...T[]]`). TypeScript does not narrow on
 * `.length` checks, so this guard is the runtime bridge:
 *
 * ```ts
 * if (isNonEmpty(xs)) head(xs); // xs: [T, ...T[]] here
 * ```
 */
export function isNonEmpty<T>(xs: readonly T[]): xs is [T, ...T[]] {
  return xs.length > 0;
}

/**
 * Type-level machinery for zts `constrict` assertions (Phase 7) —
 * type-only exports, fully erased. Not for application code: the zts
 * compiler references these from the lowering of `constrict A == B;`
 * (`type __ztsConstrict = __ztsExpect<__ztsEqual<A, B>>;`). A false
 * claim fails `__ztsExpect`'s constraint (TS2344), remapped to the
 * assert's own line.
 *
 * `__ztsEqual` is the exact-equality trick (conditional-type identity
 * over function signatures): it distinguishes branded newtypes, `any`
 * vs `unknown`, and optionality — mutual `extends` would not.
 */
export type __ztsExpect<T extends true> = T;
export type __ztsEqual<X, Y> =
  (<T>() => T extends X ? 1 : 2) extends <T>() => T extends Y ? 1 : 2
    ? true
    : false;
export type __ztsNot<B extends boolean> = B extends true ? false : true;

/**
 * The range-arm predicate the zts compiler references for `lo..=hi` match
 * patterns (0.5.0). Not for application code.
 *
 * `400..=499 => …` lowers to
 * `if (__ztsInRange<__ztsRange0>(__m, 400, 499)) { … }`, where
 * `__ztsRange0` is a hoisted, fully erased `400 | 401 | … | 499`. A type
 * PREDICATE narrows without comparing, so the arm cannot produce the
 * TS2678/TS2367 that a `switch`/`===` expansion would emit for every
 * enumerated value outside the scrutinee's type — and the negative branch
 * still removes the covered members, so the exhaustiveness keystone
 * discharges over a closed numeric literal union.
 *
 * Two details are load-bearing:
 * - the parameter is `unknown`, not `number`: the scrutinee may be a mixed
 *   union (`42 | "unknown"`), and a `number` parameter would reject it;
 * - the integer gate is `% 1 === 0`, not `Number.isInteger`: the latter is
 *   ES2015 and would raise the emitted-TS lib floor (same reasoning as
 *   `indexOf` over `includes` in the `union` guard).
 */
export const __ztsInRange = <T extends number>(
  __v: unknown,
  __lo: number,
  __hi: number,
): __v is T =>
  typeof __v === "number" && __v % 1 === 0 && __v >= __lo && __v <= __hi;

/**
 * The exhaustiveness keystone the zts compiler references in
 * committed-twins mode (`import { __ztsAbsurd } from "@zestty/core"`).
 * Not for application code: if this ever throws, a `match` was compiled
 * against a stale type. The unmatched value rides on `ztsTag` —
 * deliberately NOT `kind`, so the thrown object can never impersonate a
 * domain tagged union in a `catch`.
 */
export function __ztsAbsurd(x: never): never {
  throw Object.assign(new globalThis.Error("zts: non-exhaustive match"), {
    ztsTag: x,
  });
}
