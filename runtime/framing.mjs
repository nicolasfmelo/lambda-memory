export const MAX_REQUEST_BYTES = 64 * 1024;
export const MAX_RESPONSE_BYTES = 1024 * 1024;

export async function* frames(stream, maximum) {
  const buffer = Buffer.allocUnsafe(maximum);
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let used = 0;
  for await (const incoming of stream) {
    const chunk = Buffer.isBuffer(incoming) ? incoming : Buffer.from(incoming);
    let start = 0;
    while (start < chunk.length) {
      const newline = chunk.indexOf(10, start);
      const end = newline < 0 ? chunk.length : newline + 1;
      const length = end - start;
      if (used + length > maximum) throw new Error('frame bound');
      chunk.copy(buffer, used, start, end); used += length;
      if (newline >= 0) {
        yield decoder.decode(buffer.subarray(0, used - 1));
        used = 0;
      }
      start = end;
    }
  }
  if (used) throw new Error('incomplete frame');
}

export function writeFrame(stream, value, maximum) {
  return writeRawFrame(stream, JSON.stringify(value), maximum);
}

export async function writeRawFrame(stream, text, maximum) {
  const bytes = Buffer.from(`${text}\n`);
  if (bytes.length > maximum || stream.destroyed || stream.writableEnded) throw new Error('output unavailable');
  if (stream.write(bytes)) return;
  await new Promise((resolve, reject) => {
    const cleanup = () => { stream.off('drain', drain); stream.off('close', close); stream.off('error', error); };
    const drain = () => { cleanup(); resolve(); };
    const close = () => { cleanup(); reject(new Error('output closed')); };
    const error = () => { cleanup(); reject(new Error('output unavailable')); };
    stream.once('drain', drain); stream.once('close', close); stream.once('error', error);
    if (stream.destroyed) close();
  });
}
