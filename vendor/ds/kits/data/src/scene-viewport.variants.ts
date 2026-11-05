import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";
export const sceneViewport = tv(
  {
    slots: {
      annotations: "pointer-events-none absolute inset-0 overflow-hidden",
      annotation:
        "pointer-events-auto absolute grid w-max max-w-full min-h-[var(--reddb-spatial-target-min)] min-w-[var(--reddb-spatial-target-min)] gap-[var(--reddb-spatial-gap-sm)] rounded-[var(--reddb-radius-md)] border border-control-edge bg-elevation-raised-surface px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-inset-sm)] text-start text-foreground shadow-sm [overflow-wrap:anywhere] data-[selected=true]:border-2 data-[selected=true]:border-foreground [contain:layout]",
      label: "max-w-[24ch] text-caption font-medium",
      description: "max-w-[24ch] text-caption text-ink-muted",
      followControls:
        "mt-[var(--reddb-spatial-gap-sm)] flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
      note: "text-caption text-ink-muted",
    },
  },
  typeRoleMerge,
);
