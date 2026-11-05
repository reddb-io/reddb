import type { AvatarSize } from "../avatar.variants";

/** Identity and appearance shared by the native Avatar adapters. */
export interface AvatarContract {
  /** Optional image source. Omitting it renders the fallback immediately. */
  src?: string;
  /** Accessible name for the person or entity represented. */
  name: string;
  /** Visible text required when no icon alternative is supplied. */
  fallback?: string;
  /** Density-responsive avatar size. */
  size?: AvatarSize;
  /** Extra classes merged onto the image. */
  imageClass?: string;
  /** Extra classes merged onto the fallback layer. */
  fallbackClass?: string;
}

/** A failure belongs to its source; a replacement image gets its own attempt. */
export function avatarImageVisible(src: string | undefined, failedSrc: string | undefined): src is string {
  return Boolean(src) && src !== failedSrc;
}

/** The browser may finish a failed SSR image before hydration installs handlers. */
export function avatarImageFailed(image: HTMLImageElement | null | undefined): boolean {
  return Boolean(image?.complete && image.naturalWidth === 0);
}
