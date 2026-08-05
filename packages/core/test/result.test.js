import { test } from "node:test";
import assert from "node:assert/strict";
import {
  Ok,
  Err,
  map,
  map_err,
  is_ok,
  is_err,
  unwrap,
  unwrap_or,
} from "../index.ts";

test("Ok/Err use the kind discriminant", () => {
  assert.deepEqual(Ok(2), { kind: "Ok", value: 2 });
  assert.deepEqual(Err("boom"), { kind: "Err", error: "boom" });
});

test("map transforms Ok and passes Err through", () => {
  assert.deepEqual(map(Ok(2), (n) => n * 3), Ok(6));
  const e = Err("boom");
  assert.equal(map(e, (n) => n), e);
});

test("map_err transforms Err and passes Ok through", () => {
  assert.deepEqual(map_err(Err(404), (c) => `code ${c}`), Err("code 404"));
  const ok = Ok(1);
  assert.equal(map_err(ok, (c) => c), ok);
});

test("guards narrow", () => {
  assert.ok(is_ok(Ok(1)) && is_err(Err(1)));
  assert.ok(!is_ok(Err(1)) && !is_err(Ok(1)));
});

test("unwrap returns or throws", () => {
  assert.equal(unwrap(Ok(7)), 7);
  assert.throws(() => unwrap(Err(new RangeError("nope"))), RangeError);
  assert.equal(unwrap_or(Err("x"), 9), 9);
});

test("composes with the match lowering convention (kind checks)", () => {
  // Simulates what a lowered zts match does with a Result.
  const r = map(Ok(21), (n) => n * 2);
  const __k = r.kind;
  let out;
  if (__k === "Ok") out = r.value;
  else out = -1;
  assert.equal(out, 42);
});
