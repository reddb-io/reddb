<script lang="ts">
  import { Container, SkipLink, Stack } from "@reddb-io/design-system/base";
</script>

<SkipLink />
<main id="main-content" tabindex="-1">
  <Container aria-label="Deployment content">
    <Stack gap="lg" aria-label="Deployment flow">
      <h1>Deployment</h1>
      <div data-density="compact">
        <Container aria-label="Compact nested content">
          <Stack gap="sm"><p>Nested density island</p></Stack>
        </Container>
      </div>
    </Stack>
  </Container>
</main>
