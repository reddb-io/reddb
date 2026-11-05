import type { Snippet } from "svelte";
import type { DescriptionListEntry } from "./core/description-list";
export * from "./description-list.appearance";

export type DescriptionListContent = string | Snippet;
export interface DescriptionListItem extends DescriptionListEntry<DescriptionListContent> {}
