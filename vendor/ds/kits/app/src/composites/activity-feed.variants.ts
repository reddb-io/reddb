import { tv, type VariantProps } from "tailwind-variants";

/** Token-only extension seam over the canonical Base Timeline. */
export const activityFeed = tv({
  slots: {
    root: "m-0 w-full min-w-0 gap-[var(--reddb-spatial-gap-md)]",
  },
});

export type ActivityFeedVariants = VariantProps<typeof activityFeed>;
