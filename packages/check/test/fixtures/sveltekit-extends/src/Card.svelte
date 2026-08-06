<script lang="zts">
  const n: number = if (true as boolean) { 1 } else { 2 };
  export const label = `${n}`;
</script>

<p>{label.charAt(0)}</p>
