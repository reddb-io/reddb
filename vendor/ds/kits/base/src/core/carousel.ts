import type { ImageAsset } from "../image.variants";
/** Portable content properties of one carousel slide. */
export interface CarouselSlideContract {
  /** Visible meaning included in the slide's accessible position label. */
  label: string;
  /** Base Image used when the adapter has no custom slide content. */
  image?: ImageAsset;
}
