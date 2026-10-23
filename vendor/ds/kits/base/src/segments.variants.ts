import { tv } from "tailwind-variants";
export const segments = tv({
  slots: {
    track: "grid w-full gap-[var(--reddb-spatial-gap-sm)]",
    cell: "h-[var(--reddb-spatial-gap-md)] min-w-0 overflow-hidden bg-muted",
    fill: "h-full",
  },
});
