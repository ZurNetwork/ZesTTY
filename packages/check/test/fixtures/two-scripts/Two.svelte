<script context="module" lang="zts">
  export const fine: number = 1;
</script>

<script lang="zts">
  type T = { kind: "K"; v: number } | { kind: "L"; w: number };
  const t: T = { kind: "K", v: 1 } as T;
  export const broken = match (t) {
    K { v } => v,
  };
</script>

<p>two</p>
