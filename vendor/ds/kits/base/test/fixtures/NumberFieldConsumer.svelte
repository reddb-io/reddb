<script lang="ts">
  import { Button, NumberField } from "@reddb-io/design-system/base";
  let quantity = $state<number|null>(3);
</script>
<form>
  <NumberField label="Units" name="units" bind:value={quantity} min={1} max={12}/>
  <Button onclick={()=>quantity=null}>Clear externally</Button>
  <p data-bound-value>{quantity===null?"empty":quantity}</p>
</form>
