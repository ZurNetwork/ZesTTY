<script lang="zts">
  enum Price {
    Fixed { amount: number },
    Free {},
  }

  const p = Price.Fixed(9.99);

  export const label: string = match (p) {
    Fixed { amount } => `$${amount}`,
    Free {} => "free",
  };
</script>

<p>{label.toFixed(2)}</p>
