<p>widget</p>

<script lang="zts">
  type State = { kind: "A"; n: number } | { kind: "B"; s: string };
  declare const st: State;

  export const label = match (st) {
    A { n } => `${n}`,
  };
</script>
