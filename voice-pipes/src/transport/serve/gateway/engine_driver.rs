//! The `EngineDriver` seam: the single, documented surface describing what
//! legitimately differs between the classic (STT -> LLM -> TTS) and realtime
//! (provider-native) engines.
//!
//! ## Why a trait and not a forced merge
//!
//! The two runners share most of their orchestration (state transitions,
//! ping/timing handling, the control-message wire shapes). Those shared
//! parts now live in [`super::state_machine`] and [`super::shared_handlers`].
//! What remains genuinely engine-specific is captured here as a small trait
//! so that:
//!
//! - adding a future engine is a compile error until it implements this
//!   trait (extensibility with no silent omissions), and
//! - the divergent logic is isolated and individually testable instead of
//!   being interleaved with the shared flow (reliability).
//!
//! ## Scope: the two operations that genuinely unify
//!
//! 1. [`EngineDriver::cancel_turn`] — abort the in-flight turn at the engine
//!    boundary. Classic toggles a `CancellationToken`; realtime sends a
//!    provider cancel envelope via `RealtimeClient::cancel`.
//! 2. [`EngineDriver::resolve_played_text`] — given a client playback offset,
//!    recover the assistant text actually heard. Classic consults the live
//!    `AudioLedger`; realtime binary-searches the frozen `text_timeline`
//!    captured at interruption time.
//!
//! ## Intentionally NOT in the seam: turn-metrics finalization
//!
//! Turn-metrics finalization deliberately stays out of this trait. Forcing it
//! in would be a leaky abstraction:
//!
//! - The classic finalizer waits up to a grace period on the pipeline metrics
//!   channel and then amends the snapshot with runner-observed fields; the
//!   realtime finalizer builds the snapshot synchronously from `RealtimeTurn`
//!   and, on interruption, *also* produces a `PendingInterruption` that a
//!   single `-> TurnMetricsSnapshot` signature would silently drop.
//! - The two finalizers are reached from five call sites (classic: cancel,
//!   server barge-in, text interrupt; realtime: provider-completed, cancel),
//!   each with different surrounding orchestration, and only the realtime
//!   cancel path currently has regression coverage. The golden net covers the
//!   happy path only.
//!
//! Unifying metrics finalization is therefore deferred until interrupt-path
//! golden fixtures exist, at which point it can be folded in behind the same
//! net that protects the happy path today.

/// The per-engine operations that legitimately differ between the classic and
/// realtime runners. Implemented by each runner's session context so a new
/// engine must provide both operations or fail to compile.
pub(crate) trait EngineDriver {
    /// Cancel the in-flight turn at the engine boundary.
    ///
    /// Classic discards and fires its `CancellationToken` (infallible).
    /// Realtime forwards a provider cancel envelope over the network, which
    /// can fail; the error is propagated so the caller can surface it.
    async fn cancel_turn(&mut self) -> anyhow::Result<()>;

    /// Resolve the assistant text the client actually heard for
    /// `utterance_id` at the given playback `offset_ms`.
    ///
    /// Returns `None` when no text can be resolved (e.g. unknown utterance,
    /// no ledger/timeline, or a zero offset). An empty string is a distinct,
    /// meaningful result: "the client played none of the assistant audio".
    fn resolve_played_text(&self, utterance_id: u32, offset_ms: u32) -> Option<String>;
}
