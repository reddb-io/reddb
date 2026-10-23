<!--
  Deliberately broken public DOM fixtures. They are counter-examples, not
  alternate components: the accessibility suite must diagnose each omission
  before the real Field/Input tracer is allowed to pass.
-->
<script lang="ts">
  interface Props {
    failure: "missing-label" | "missing-error-association" | "invalid-native-forwarding";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "missing-label"}
  <div data-a11y-fixture={failure}>
    <input name="email" type="email" />
  </div>
{:else if failure === "missing-error-association"}
  <div data-a11y-fixture={failure}>
    <label for="broken-email">Email address</label>
    <input id="broken-email" name="email" type="email" aria-invalid="true" />
    <p id="broken-email-error">Enter a valid email address.</p>
  </div>
{:else}
  <div data-a11y-fixture={failure}>
    <!-- The wrapper was asked for native constraints but dropped them. -->
    <input data-intended-required="true" data-intended-pattern=".+@.+" name="email" />
  </div>
{/if}
