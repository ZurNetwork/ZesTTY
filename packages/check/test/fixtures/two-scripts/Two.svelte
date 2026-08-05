<script context="module" lang="zts">
  export const fine: number = 1;
</script>

<script lang="zts">
  type T = { kind: "K"; v: number } | { kind: "L"; w: number };
  declare const t: T;
  export const broken = match (t) {
    K { v } => v,
  };
</script>

<p>two</p>
