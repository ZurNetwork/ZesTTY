import { test } from "node:test";
import assert from "node:assert/strict";
import { preprocess } from "svelte/compiler";
import ztsPreprocess from "@zestty/svelte-preprocess";

const COMPONENT = `<script lang="zts">
  type State =
    | { kind: "Loading" }
    | { kind: "Ready"; items: string[] };
  export let state: State;
  const label = match (state) {
    Loading {} => "…",
    Ready { items } => \`\${items.length} items\`,
  };
</script>

<p>{label}</p>
`;

test('compiles <script lang="zts"> and rewrites lang to ts', async () => {
  const result = await preprocess(COMPONENT, [ztsPreprocess()], {
    filename: "Widget.svelte",
  });
  assert.match(result.code, /lang="ts"/);
  assert.match(result.code, /__ztsAbsurd/);
  assert.doesNotMatch(result.code, /match \(state\)/);
  assert.match(result.code, /<p>\{label\}<\/p>/);
});

test('leaves lang="ts" scripts alone', async () => {
  const src = `<script lang="ts">const n: number = 1;</script><p>{n}</p>`;
  const result = await preprocess(src, [ztsPreprocess()], {
    filename: "T.svelte",
  });
  assert.equal(result.code, src);
});

test("leaves untyped scripts alone", async () => {
  const src = `<script>const n = 1;</script><p>{n}</p>`;
  const result = await preprocess(src, [ztsPreprocess()], {
    filename: "U.svelte",
  });
  assert.equal(result.code, src);
});
