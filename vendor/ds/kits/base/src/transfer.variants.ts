import { tv } from "tailwind-variants";
/** Invisible lifecycle anchor shared by the native renderers. */
export const transfer = tv({
  base: "pointer-events-none absolute size-0 overflow-hidden",
});
