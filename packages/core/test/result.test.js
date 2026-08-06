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
} from "../dist/index.js";

test("Ok/Err use the kind discriminant", () => {
  assert.deepEqual(Ok(2), { kind: "Ok", value: 2 });
  assert.deepEqual(Err("boom"), { kind: "Err", error: "boom" });
});

test("map transforms Ok and passes Err through", () => {
  assert.deepEqual(
    map(Ok(2), (n) => n * 3),
    Ok(6),
  );
  const e = Err("boom");
  assert.equal(
    map(e, (n) => n),
    e,
  );
});

test("map_err transforms Err and passes Ok through", () => {
  assert.deepEqual(
    map_err(Err(404), (c) => `code ${c}`),
    Err("code 404"),
  );
  const ok = Ok(1);
  assert.equal(
    map_err(ok, (c) => c),
    ok,
  );
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

test("and_then chains Result-returning fns and passes Err through", async () => {
  const { and_then } = await import("../dist/index.js");
  assert.deepEqual(
    and_then(Ok(2), (n) => Ok(n * 5)),
    Ok(10),
  );
  assert.deepEqual(
    and_then(Ok(2), () => Err("inner")),
    Err("inner"),
  );
  const e = Err("outer");
  assert.equal(
    and_then(e, () => Ok(1)),
    e,
  );
});

test("ResultPipe chains left-to-right and returns plain data", async () => {
  const { ResultPipe } = await import("../dist/index.js");
  const out = ResultPipe(Ok(2))
    .map((n) => n + 1)
    .and_then((n) => (n > 0 ? Ok(n * 10) : Err("neg")))
    .map_err((e) => `wrapped: ${e}`)
    .done();
  assert.deepEqual(out, Ok(30));
  // done() returns a PLAIN tagged object — survives structuredClone.
  assert.deepEqual(structuredClone(out), Ok(30));
  assert.equal(Object.getPrototypeOf(out), Object.prototype);

  const err = ResultPipe(Err("boom"))
    .map((n) => n + 1)
    .map_err((e) => `wrapped: ${e}`)
    .done();
  assert.deepEqual(err, Err("wrapped: boom"));
});

test("ResultPipe unwrap terminals", async () => {
  const { ResultPipe } = await import("../dist/index.js");
  assert.equal(
    ResultPipe(Ok(7))
      .map((n) => n + 1)
      .unwrap(),
    8,
  );
  assert.equal(ResultPipe(Err("boom")).unwrap_or(42), 42);
  assert.throws(() => ResultPipe(Err("boom")).unwrap(), /boom/);
});
