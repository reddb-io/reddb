// A stand-in component for `markers.test.ts`, not part of the Kit.
//
// Its vocabulary is chosen to break a marker map in the way that matters:
// `inline-flex rounded-full border` is an idiom `fixture-chip` writes too, so
// a marker cut from it names both components, while `fixture-badge-cap` is
// this component's alone and names one.

import { tv } from "tailwind-variants";

export const fixtureBadge = tv({
  base: "fixture-badge-cap inline-flex rounded-full border",
});
