/**
 * Encode 16-bit mono PCM as a WAV file.
 *
 * The probes take a WAV in the request body, and a recording made in the
 * browser is a list of `Int16Array` blocks from the capture path. This is the
 * whole of the conversion: a 44-byte RIFF header in front of the samples.
 */
export function encodeWav(samples: Int16Array, sampleRate: number): ArrayBuffer {
  const dataBytes = samples.length * 2;
  const buf = new ArrayBuffer(44 + dataBytes);
  const v = new DataView(buf);
  const ascii = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(off + i, s.charCodeAt(i));
  };

  ascii(0, "RIFF");
  v.setUint32(4, 36 + dataBytes, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  v.setUint32(16, 16, true);            // fmt chunk size
  v.setUint16(20, 1, true);             // PCM
  v.setUint16(22, 1, true);             // mono
  v.setUint32(24, sampleRate, true);
  v.setUint32(28, sampleRate * 2, true); // byte rate
  v.setUint16(32, 2, true);             // block align
  v.setUint16(34, 16, true);            // bits per sample
  ascii(36, "data");
  v.setUint32(40, dataBytes, true);

  // Little-endian by construction rather than by trusting the host: a
  // big-endian platform copying the typed array's bytes would write a file
  // that plays as static.
  for (let i = 0; i < samples.length; i++) {
    v.setInt16(44 + i * 2, samples[i], true);
  }
  return buf;
}

/** Join capture blocks into one buffer. */
export function concatBlocks(blocks: Int16Array[]): Int16Array {
  const total = blocks.reduce((n, b) => n + b.length, 0);
  const out = new Int16Array(total);
  let off = 0;
  for (const b of blocks) {
    out.set(b, off);
    off += b.length;
  }
  return out;
}
