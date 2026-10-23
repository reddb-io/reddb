/** Load as the figure nears the viewport, or immediately after mounting. */
export type ChartLoading = "lazy" | "eager";
/** Begin loading outside the viewport so marks are ready when they scroll into view. */
export const CHART_LOAD_MARGIN = "320px";
/** Renderer-neutral visibility observer; environments without layout load immediately. */
export function whenNear(element: Element, near: () => void): () => void {
 if(typeof IntersectionObserver==="undefined"){near();return()=>{};}
 const observer=new IntersectionObserver(entries=>{if(!entries.some(entry=>entry.isIntersecting))return;observer.disconnect();near();},{rootMargin:CHART_LOAD_MARGIN});observer.observe(element);return()=>observer.disconnect();
}
