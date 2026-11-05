import { tv } from "tailwind-variants";
import type { ComboboxOption } from "./combobox.variants";

export interface SearchSelectOption extends ComboboxOption {
  /** Supporting text shown beneath the option label and included in search. */
  description?: string;
  /** Additional caller-owned search terms, such as codes or aliases. */
  keywords?: readonly string[];
}

/** Accent-insensitive search over labels, descriptions and explicit aliases. */
export function searchSelectOptions(options: readonly SearchSelectOption[], query: string): readonly SearchSelectOption[] {
  const normalize = (text: string) => text.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLocaleLowerCase();
  const terms = normalize(query).trim().split(/\s+/).filter(Boolean);
  return terms.length ? options.filter(option => {
    const text = normalize([option.label, option.description, ...(option.keywords ?? [])].filter(Boolean).join(" "));
    return terms.every(term => text.includes(term));
  }) : options;
}

export const searchSelect = tv({
  slots: {
    root: "min-w-0",
    row: "flex min-w-0 items-center gap-[var(--reddb-spatial-gap-sm)]",
    trigger: "min-w-0 flex-1 justify-between text-start font-regular",
    summary: "min-w-0 truncate",
    placeholder: "text-ink-muted",
    content: "w-[max(20rem,var(--bits-popover-anchor-width))] max-w-[calc(100vw-2*var(--reddb-spatial-inset-md))] max-h-[min(70dvh,var(--bits-popover-content-available-height))] overflow-y-auto min-w-0 p-[var(--reddb-spatial-inset-sm)]",
    search: "sticky top-0 z-10 mb-[var(--reddb-spatial-gap-sm)]",
    list: "min-w-0",
    item: "group flex min-h-[var(--reddb-spatial-control-height-md)] cursor-default items-center justify-between gap-[var(--reddb-spatial-gap-md)] rounded-md px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-inset-sm)] text-body text-foreground data-[highlighted]:bg-foreground/10 data-[selected]:font-medium data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50",
    option: "min-w-0 break-words",
    description: "block text-caption font-regular text-ink-muted group-data-[highlighted]:text-foreground",
    empty: "block px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-inset-md)] text-body text-ink-muted",
    chips: "m-0 flex list-none flex-wrap gap-[var(--reddb-spatial-gap-sm)] p-0",
    chip: "max-w-full justify-between gap-[var(--reddb-spatial-gap-sm)] rounded-md bg-muted text-foreground",
    clear: "aspect-square shrink-0 p-0",
  },
});
