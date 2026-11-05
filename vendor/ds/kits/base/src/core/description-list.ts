/** A term/detail association; each renderer supplies its native content type. */
export interface DescriptionListEntry<Content> {
  term: Content;
  detail: Content;
}
