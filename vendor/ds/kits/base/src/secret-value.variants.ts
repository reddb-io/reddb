import { tv } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";
export const secretValue = tv(
  {
    slots: {
      root: "@container/secret-value grid min-w-0 gap-[var(--reddb-spatial-gap-sm)] text-foreground",
      label: "text-caption font-medium [overflow-wrap:anywhere]",
      row: "flex min-w-0 flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
      value:
        "min-w-0 flex-1 whitespace-pre-wrap font-mono text-body [overflow-wrap:anywhere]",
      error:
        "m-0 text-caption text-feedback-danger-foreground [overflow-wrap:anywhere]",
    },
  },
  typeRoleMerge,
);
