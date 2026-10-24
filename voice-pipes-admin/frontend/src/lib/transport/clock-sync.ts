/** NTP-lite clock offset estimator using Ping/Pong round-trips.
 *
 *  Uses `performance.now()` for all duration measurements (monotonic)
 *  and `Date.now()` only to anchor samples to wall time.
 *
 *  Maintains a rolling window of samples, using the **median** offset
 *  (robust against GC jitter and network outliers). */
export class ClockSync {
  private samples: { offset: number }[] = [];
  private readonly windowSize: number;

  /** Best-estimate offset: server_time ≈ client_time + offset. */
  offset = 0;

  constructor(windowSize = 10) {
    this.windowSize = windowSize;
  }

  /** True once at least one Pong sample has been fed — before that `offset`
   *  is the placeholder 0 and must not be used to correct timestamps. */
  get hasSamples(): boolean {
    return this.samples.length > 0;
  }

  /** Feed a completed Pong round-trip.
   *  @param clientSentPerf  `performance.now()` when Ping was sent
   *  @param serverTs        `server_timestamp_ms` from the Pong
   *  @param clientRecvPerf  `performance.now()` when Pong arrived
   *  @param clientRecvWall  `Date.now()` sampled at the same instant as clientRecvPerf */
  addSample(clientSentPerf: number, serverTs: number, clientRecvPerf: number, clientRecvWall: number): void {
    const rtt = clientRecvPerf - clientSentPerf;
    const oneWay = rtt / 2;
    // Anchor: the server timestamp corresponds to approximately
    // clientRecvWall - oneWay on the client wall clock.
    const offset = serverTs - (clientRecvWall - oneWay);

    this.samples.push({ offset });
    if (this.samples.length > this.windowSize) {
      this.samples.shift();
    }

    // Median
    const sorted = this.samples.map((s) => s.offset).sort((a, b) => a - b);
    this.offset = sorted[Math.floor(sorted.length / 2)];
  }

  /** Convert a client-side wall-clock timestamp to approximate server time. */
  toServerTime(clientWallMs: number): number {
    return clientWallMs + this.offset;
  }
}
