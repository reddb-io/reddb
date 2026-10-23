/** Fixed-height rows measured in Density control rows. No DOM or renderer. */
export function virtualRange(total: number, rowHeight: number, viewportHeight: number, scrollTop: number, overscan: number) {
  if (!Number.isSafeInteger(total) || total < 0 || !Number.isFinite(rowHeight) || rowHeight <= 0 ||
      !Number.isFinite(viewportHeight) || viewportHeight < 0 || !Number.isFinite(scrollTop) ||
      !Number.isSafeInteger(overscan) || overscan < 0) throw new Error("VirtualList: invalid row geometry");
  const top = Math.max(0, Math.min(scrollTop, Math.max(0, total * rowHeight - viewportHeight)));
  return { start: Math.max(0, Math.floor(top / rowHeight) - overscan),
    end: Math.min(total, Math.ceil((top + viewportHeight) / rowHeight) + overscan), top };
}

/** Validate stable keys and Density-relative viewport configuration identically in both adapters. */
export function virtualKeys<T>(items:readonly T[],rowKey:(item:T)=>string,rowUnits:number,visibleRows:number):string[]{
 const keys=items.map(rowKey);
 if(keys.some(key=>!key)||new Set(keys).size!==keys.length)throw new Error("VirtualList: row keys must be nonempty and unique");
 if(!Number.isFinite(rowUnits)||rowUnits<=0||!Number.isSafeInteger(visibleRows)||visibleRows<1)throw new Error("VirtualList: rowUnits and visibleRows must be positive");
 return keys;
}
