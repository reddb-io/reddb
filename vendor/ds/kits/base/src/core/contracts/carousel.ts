
/** Shared canonical Carousel properties. */
export interface CarouselContract {
/** Accessible name for the carousel region. */
    label: string;
/** Initially selected zero-based slide index. */
    initialIndex?: number;
/** Opt-in automatic rotation, suspended under reduced motion. A pause/resume control is always rendered when enabled. */
    autoplay?: boolean;
/** Milliseconds between automatic selections. */
    interval?: number;
/**
     * The slide shown first holds the page's Largest Contentful Paint: its `image` is fetched
     * eagerly at high priority. Every other slide image stays lazy.
     */
    priority?: boolean;
/** Extra classes merged onto the viewport that wraps the slides. */
    viewportClass?: string;
/** Extra classes merged onto each slide's group element. */
    slideClass?: string;
/** Extra classes merged onto the row holding the previous, next and rotation controls. */
    controlsClass?: string;
}
