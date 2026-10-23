// The Theme's type roles as the Kit's class merger must see them (ADR 0025).
//
// tailwind-variants resolves conflicting classes with tailwind-merge, which
// only knows Tailwind's default type scale. To it, `text-display` is an
// unknown text utility and so a colour — it would silently drop either the role or
// `text-foreground` from the same element. Declaring the roles as font sizes
// keeps both, and lets a caller's `class="text-caption"` replace a role
// rather than stack on it.

/** The Theme's type role utilities (text-display … text-eyebrow), by role name. */
export const TYPE_ROLE_UTILITIES = ["display", "title", "heading", "body", "caption", "eyebrow"] as const;

/** The tailwind-variants config every type-role recipe passes as its second argument. */
export const typeRoleMerge = {
  twMergeConfig: {
    extend: {
      classGroups: {
        "font-size": [{ text: [...TYPE_ROLE_UTILITIES, "editable-sm", "editable-md", "editable-lg"] }],
      },
    },
  },
};
