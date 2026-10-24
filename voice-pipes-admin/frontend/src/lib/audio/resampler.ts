// Streaming sample-rate conversion for PCM i16 audio.
//
// Used on the capture path so the rate we put on the wire is the rate we
// promised, regardless of what rate the browser actually gave the
// `AudioContext` (Chrome and Safari both ignore `sampleRate` hints on some
// devices). The playback path will use the same class when the server gains
// an adaptive output rate.
//
// Two properties matter and are covered by the tests:
//
//  1. **Anti-aliasing.** Decimating without a low-pass folds everything above
//     the new Nyquist back into the audible band as tones that were never
//     there — which STT hears as noise. A 4th-order Butterworth runs ahead of
//     the decimation whenever we are reducing the rate.
//  2. **Continuity across blocks.** Audio arrives in ~128-sample worklet
//     blocks. Filter state and the fractional read position carry across
//     blocks, so there is no discontinuity at the seams. Resampling each
//     block independently is the obvious implementation and is audibly wrong.

/** Cutoff as a fraction of the output Nyquist. 0.9 keeps the passband intact
 *  for speech while leaving the filter room to roll off before it folds. */
const CUTOFF_FRACTION_OF_NYQUIST = 0.9;

/** Q values for a 4th-order Butterworth as two cascaded biquads. */
const BUTTERWORTH_Q = [0.5411961, 1.3065630];

/** One biquad section, transposed direct form II. */
class Biquad {
  private readonly b0: number;
  private readonly b1: number;
  private readonly b2: number;
  private readonly a1: number;
  private readonly a2: number;
  private z1 = 0;
  private z2 = 0;

  /** Low-pass, RBJ audio-EQ cookbook coefficients. */
  constructor(cutoffHz: number, sampleRate: number, q: number) {
    const w0 = (2 * Math.PI * cutoffHz) / sampleRate;
    const cosW = Math.cos(w0);
    const sinW = Math.sin(w0);
    const alpha = sinW / (2 * q);
    const a0 = 1 + alpha;
    this.b0 = ((1 - cosW) / 2) / a0;
    this.b1 = (1 - cosW) / a0;
    this.b2 = ((1 - cosW) / 2) / a0;
    this.a1 = (-2 * cosW) / a0;
    this.a2 = (1 - alpha) / a0;
  }

  process(x: number): number {
    const y = this.b0 * x + this.z1;
    this.z1 = this.b1 * x - this.a1 * y + this.z2;
    this.z2 = this.b2 * x - this.a2 * y;
    return y;
  }

  reset(): void {
    this.z1 = 0;
    this.z2 = 0;
  }
}

/**
 * Converts a stream of PCM i16 blocks from `inputRate` to `outputRate`,
 * carrying filter and phase state across calls.
 *
 * Feed it whatever block sizes arrive; the output block length varies by a
 * sample either way as the fractional phase advances, which is expected.
 */
export class StreamingResampler {
  private readonly ratio: number;
  private readonly filters: Biquad[];
  /** Last filtered sample of the previous block, for interpolation across
   *  the block boundary. */
  private prev = 0;
  /** Read position of the next output sample, in input samples, relative to
   *  `prev` (index 0). Starts at 1 so the first output is the first real
   *  sample rather than the zero-initialised `prev`. */
  private pos = 1;

  // Plain fields, not constructor parameter properties: the build runs with
  // `erasableSyntaxOnly`, which forbids syntax that emits runtime code.
  readonly inputRate: number;
  readonly outputRate: number;

  constructor(inputRate: number, outputRate: number) {
    this.inputRate = inputRate;
    this.outputRate = outputRate;
    if (inputRate <= 0 || outputRate <= 0) {
      throw new Error(`invalid rates: ${inputRate} -> ${outputRate}`);
    }
    this.ratio = inputRate / outputRate;
    // Only meaningful when reducing the rate; upsampling cannot alias.
    this.filters =
      this.ratio > 1
        ? BUTTERWORTH_Q.map(
            (q) =>
              new Biquad(
                (outputRate / 2) * CUTOFF_FRACTION_OF_NYQUIST,
                inputRate,
                q,
              ),
          )
        : [];
  }

  /** True when input and output rates match and `process` is a pass-through. */
  get isPassthrough(): boolean {
    return this.ratio === 1;
  }

  /** Drop all state. Call at a discontinuity (a new utterance, a restart) so
   *  stale samples cannot bleed across the boundary. */
  reset(): void {
    for (const f of this.filters) f.reset();
    this.prev = 0;
    this.pos = 1;
  }

  process(input: Int16Array): Int16Array {
    if (this.ratio === 1) return input;
    if (input.length === 0) return input;

    // work[0] is the previous block's tail, so interpolation at the seam has
    // both of its neighbours.
    const work = new Float32Array(input.length + 1);
    work[0] = this.prev;
    for (let i = 0; i < input.length; i++) {
      let s = input[i];
      for (const f of this.filters) s = f.process(s);
      work[i + 1] = s;
    }

    // Emit while both interpolation neighbours are inside `work`.
    const out: number[] = [];
    let p = this.pos;
    while (p + 1 < work.length) {
      const i = Math.floor(p);
      const frac = p - i;
      const s = work[i] * (1 - frac) + work[i + 1] * frac;
      out.push(s < -32768 ? -32768 : s > 32767 ? 32767 : Math.round(s));
      p += this.ratio;
    }

    this.prev = work[work.length - 1];
    // Rebase: the next block's work[0] is this block's last sample.
    this.pos = p - input.length;

    return Int16Array.from(out);
  }
}

/**
 * Convenience for one-shot conversion of a complete buffer. Not for streams —
 * it has no state to carry, so calling it per block would put a discontinuity
 * at every seam.
 */
export function resampleBuffer(
  input: Int16Array,
  inputRate: number,
  outputRate: number,
): Int16Array {
  return new StreamingResampler(inputRate, outputRate).process(input);
}
