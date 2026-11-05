<script lang="ts">
  import {
    BrowserMockup,
    PhoneMockup,
    WindowMockup,
  } from "@reddb-io/design-system/base";
</script>

<div
  data-mockup-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <BrowserMockup address="https://example.test/deployments">
    <article aria-label="Browser deployment">
      <button type="button">Inspect deployment</button>
    </article>
  </BrowserMockup>

  <PhoneMockup>
    <nav aria-label="Phone queue">
      <a href="/queue">Open queue</a>
    </nav>
  </PhoneMockup>

  <WindowMockup title="Logs">
    <section aria-label="Window logs">
      <pre>pnpm test
all green</pre>
      <button type="button">Copy logs</button>
    </section>
  </WindowMockup>
</div>
