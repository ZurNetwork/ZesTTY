<p>widget</p>

<script lang="zts">
  type State = { kind: "A"; n: number } | { kind: "B"; s: string };
  const st: State = { kind: "A", n: 1 } as State;

  export const label = match (st) {
    A { n } => `${n}`,
  };
</script>
