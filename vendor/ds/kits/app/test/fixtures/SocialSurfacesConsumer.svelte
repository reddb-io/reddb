<script lang="ts">
  import {
    ActivityFeed,
    ChatMessage,
    Review,
    type ActivityFeedItem,
  } from "@reddb-io/design-system/app";

  const activity: readonly ActivityFeedItem[] = [
    {
      id: "deployed",
      title: "Ada deployed the release",
      datetime: "2026-08-08T19:05:00Z",
      time: "19:05",
      href: "/deployments/42",
    },
  ];
</script>

<section
  data-social-surfaces-consumer
  data-theme="application"
  data-color-scheme="dark"
  data-density="compact"
  data-motion="reduced"
>
  <ActivityFeed label="Deployment activity" items={activity} />

  <ChatMessage
    author="Ada Lovelace"
    avatarFallback="AL"
    datetime="2026-08-08T19:10:00Z"
    time="19:10"
  >
    <p>Deployment is ready.</p>
    {#snippet actions()}<button type="button">Reply</button>{/snippet}
  </ChatMessage>

  <Review author="Grace Hopper" avatarFallback="GH" rating={5}>
    <p>Fast and reliable.</p>
    {#snippet actions()}<button type="button">Mark helpful</button>{/snippet}
  </Review>
</section>
