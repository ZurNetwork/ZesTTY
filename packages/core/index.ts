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
export function map<T, U, E>(r: Result<T, E>, f: (value: T) => U): Result<U, E> {
  return r.kind === "Ok" ? Ok(f(r.value)) : r;
}

/**
 * Transform the error value, passing successes through untouched.
 */
export function map_err<T, E, F>(r: Result<T, E>, f: (error: E) => F): Result<T, F> {
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
