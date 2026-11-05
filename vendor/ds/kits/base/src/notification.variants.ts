// Notification owns only the relationship between the canonical Alert and its
// canonical dismissal control. Alert continues to own Feedback Role colour;
// Button continues to own focus and control appearance.
import { tv, type VariantProps } from "tailwind-variants";

export const notification = tv({
  slots: {
    root: "flex w-full max-w-sm items-start gap-[var(--reddb-spatial-gap-sm)]",
    announcement: "min-w-0 flex-1",
    dismiss: "shrink-0",
  },
});

export type NotificationVariants = VariantProps<typeof notification>;
