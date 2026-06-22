#!/usr/bin/env node
// Snowplow tracker -> RedDB batch insert adapter.
// Translates a Snowplow `payload_data` array into rows for the
// `events` collection and POSTs them via /collections/events/bulk/rows
// (the batch insert endpoint). Referenced from
// docs/migrating-from-snowplow.md.

const REDDB_URL = process.env.REDDB_URL ?? 'http://127.0.0.1:5000';
const FLUSH_EVERY = Number(process.env.FLUSH_EVERY ?? 100);
const FLUSH_INTERVAL_MS = Number(process.env.FLUSH_INTERVAL_MS ?? 5_000);

function mapSnowplowEntry(entry) {
  const sde = entry.ue_pr ? JSON.parse(entry.ue_pr).data : null;
  return {
    fields: {
      event_id: entry.eid,                       // event_id -> primary key
      collector_tstamp: Number(entry.stm ?? entry.dtm ?? Date.now()),
      event_name: sde?.schema?.split('/')[1] ?? entry.e ?? 'unknown',
      payload: JSON.stringify(sde?.data ?? entry),
    },
  };
}

export function createAdapter({ url = REDDB_URL, flushEvery = FLUSH_EVERY } = {}) {
  let buffer = [];
  const flush = async () => {
    if (buffer.length === 0) return { ok: true, count: 0 };
    const items = buffer;
    buffer = [];
    const idempotencyKey = items.map((i) => i.fields.event_id).sort().join(',');
    const res = await fetch(`${url}/collections/events/bulk/rows`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'Idempotency-Key': idempotencyKey },
      body: JSON.stringify({ items }),
    });
    if (!res.ok) throw new Error(`batch insert failed: ${res.status} ${await res.text()}`);
    return res.json();
  };
  const timer = setInterval(() => { flush().catch(console.error); }, FLUSH_INTERVAL_MS);
  if (typeof timer.unref === 'function') timer.unref();
  return {
    track(payloadData) {
      for (const entry of payloadData) buffer.push(mapSnowplowEntry(entry));
      return buffer.length >= flushEvery ? flush() : Promise.resolve({ buffered: buffer.length });
    },
    flush,
    close() { clearInterval(timer); return flush(); },
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const sample = {
    payload_data: [
      { eid: '11111111-1111-1111-1111-111111111111', stm: '1700000000000', e: 'ue',
        ue_pr: JSON.stringify({ data: { schema: 'iglu:com.acme/page_view/jsonschema/1-0-0',
                                        data: { url: 'https://example.com' } } }) },
      { eid: '22222222-2222-2222-2222-222222222222', stm: '1700000000500', e: 'ue',
        ue_pr: JSON.stringify({ data: { schema: 'iglu:com.acme/link_click/jsonschema/1-0-0',
                                        data: { target: '/signup' } } }) },
    ],
  };
  const adapter = createAdapter({ flushEvery: 1 });
  await adapter.track(sample.payload_data);
  await adapter.close();
  console.log('flushed', sample.payload_data.length, 'events to', REDDB_URL);
}
