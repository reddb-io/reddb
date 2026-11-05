<script lang="ts">
  import { CodeBlock, Kbd, Link } from "@reddb-io/design-system/base";
</script>

<section
  data-inline-text-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <p>
    Open the <Link href="/guide">guide</Link> with <Kbd keys={["Ctrl", "K"]} />.
  </p>
  <CodeBlock code={'const greeting = "hello";'} language="TypeScript" />
</section>
