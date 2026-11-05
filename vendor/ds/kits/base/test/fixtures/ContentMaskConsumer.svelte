<!-- ContentMask at the public Base boundary, below an appearance scope owned
     entirely by the consumer and around consumer-owned semantics. -->
<script lang="ts">
  import { Button, ContentMask } from "@reddb-io/design-system/base";

  let activations = $state(0);
</script>

<div
  data-content-mask-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
  data-motion="reduced"
>
  <ContentMask shape="squircle" data-masked-image>
    <img src="/ada.png" alt="Ada Lovelace" />
  </ContentMask>

  <ContentMask shape="diamond" data-masked-control>
    <Button onclick={() => (activations += 1)}>Open masked control</Button>
  </ContentMask>

  <output data-mask-activations>{activations}</output>
</div>
