//! Shared building blocks for the local ONNX providers.
//!
//! Every in-process model provider holds a small number of pre-built ONNX
//! sessions and lends them out for the duration of one inference (or, for a
//! streaming provider, one whole utterance). The pooling, the fairness and the
//! return-on-drop behaviour are identical across them, so they live here.

use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "mel")]
pub mod mel;
#[cfg(feature = "session")]
pub mod session;

#[cfg(feature = "session")]
pub use session::{
    EXECUTION_PROVIDER_VAR, ExecutionTarget, ModelOutputError, SessionOpenError, SessionOptions,
    dim, nth_output, open_session,
};

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// How long a caller waits for a free pool item before giving up.
///
/// A streaming session holds its item for the whole utterance, so an
/// exhausted pool can otherwise block for minutes with nothing telling the
/// operator why.
pub const DEFAULT_POOL_WAIT: Duration = Duration::from_secs(5);

/// The index of the largest value, the first on a tie; NaN never wins.
/// Greedy decoding's choice of token from a model's logits.
///
/// `None` when there is nothing to choose: an empty slice, or one holding
/// only NaN. A caller turns that into an error; reading it as index 0
/// would decode a broken model output as a real token.
pub fn argmax(xs: &[f32]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &v) in xs.iter().enumerate() {
        if v.is_nan() {
            continue;
        }
        if best.is_none_or(|(_, best_v)| v > best_v) {
            best = Some((i, v));
        }
    }
    best.map(|(i, _)| i)
}

/// Every item in the pool was still checked out when the wait expired.
///
/// Providers map this into their own error type; the `Display` text is
/// written for the operator who has to act on it, so it names the knob to
/// turn rather than the internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolExhausted {
    /// What the pool holds, in operator-facing words ("Piper engines").
    pub label: &'static str,
    /// How many items the pool was built with.
    pub size: usize,
    /// How long the caller waited before giving up.
    pub waited: Duration,
}

impl std::fmt::Display for PoolExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "all {} {} are in use by other sessions (waited {} ms); \
             raise `pool_size` or run fewer sessions per worker",
            self.size,
            self.label,
            self.waited.as_millis()
        )
    }
}

impl std::error::Error for PoolExhausted {}

/// A pool was asked to be built over no items.
///
/// Such a pool would make every checkout wait out its deadline and then
/// report it was exhausted, so it is refused when built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyPool {
    /// What the pool would have held ("Piper engines").
    pub label: &'static str,
}

impl std::fmt::Display for EmptyPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a pool of {} needs at least one; set `pool_size` to 1 or more",
            self.label
        )
    }
}

impl std::error::Error for EmptyPool {}

/// Bounded pool of pre-built inference items.
///
/// A checkout waits for a free item rather than failing immediately, so
/// concurrent sessions queue for inference instead of erroring out under a
/// brief burst — but the wait is bounded, so a genuinely saturated worker
/// reports a `PoolExhausted` the caller can turn into a busy response.
pub struct SessionPool<T> {
    free: Mutex<Vec<T>>,
    semaphore: Arc<Semaphore>,
    size: usize,
    label: &'static str,
}

impl<T> std::fmt::Debug for SessionPool<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionPool")
            .field("label", &self.label)
            .field("size", &self.size)
            .field("free", &self.free.lock().len())
            .finish_non_exhaustive()
    }
}

impl<T> SessionPool<T> {
    /// Build a pool over already-constructed items; there must be at least
    /// one.
    ///
    /// # Errors
    ///
    /// [`EmptyPool`] when `items` is empty.
    pub fn new(items: Vec<T>, label: &'static str) -> Result<Self, EmptyPool> {
        if items.is_empty() {
            return Err(EmptyPool { label });
        }
        let size = items.len();
        Ok(Self {
            free: Mutex::new(items),
            semaphore: Arc::new(Semaphore::new(size)),
            size,
            label,
        })
    }

    /// Number of items the pool was built with.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Wait up to `wait` for a free item and take it behind a guard.
    pub async fn checkout_within(
        self: &Arc<Self>,
        wait: Duration,
    ) -> Result<PooledSlot<T>, PoolExhausted> {
        let permit =
            match tokio::time::timeout(wait, Arc::clone(&self.semaphore).acquire_owned()).await {
                Ok(acquired) => acquired.expect("pool semaphore is never closed"),
                Err(_elapsed) => {
                    let exhausted = PoolExhausted {
                        label: self.label,
                        size: self.size,
                        waited: wait,
                    };
                    tracing::warn!(
                        pool = self.label,
                        pool_size = self.size,
                        waited_ms = wait.as_millis() as u64,
                        "inference pool exhausted; rejecting the request instead of waiting"
                    );
                    return Err(exhausted);
                }
            };
        let item = self
            .free
            .lock()
            .pop()
            .expect("a permit guarantees a free item");
        Ok(PooledSlot {
            pool: Arc::clone(self),
            item: Some(item),
            _permit: permit,
        })
    }

    /// Wait up to [`DEFAULT_POOL_WAIT`] for a free item.
    pub async fn checkout(self: &Arc<Self>) -> Result<PooledSlot<T>, PoolExhausted> {
        self.checkout_within(DEFAULT_POOL_WAIT).await
    }

    /// Take a free item now, or `None` if every item is checked out.
    #[cfg(test)]
    pub fn try_checkout(self: &Arc<Self>) -> Option<PooledSlot<T>> {
        let permit = Arc::clone(&self.semaphore).try_acquire_owned().ok()?;
        let item = self.free.lock().pop()?;
        Some(PooledSlot {
            pool: Arc::clone(self),
            item: Some(item),
            _permit: permit,
        })
    }

    /// How many items are currently free.
    #[cfg(test)]
    pub fn available(&self) -> usize {
        self.free.lock().len()
    }
}

/// RAII guard around one pooled item. Returns the item to the pool on drop --
/// normal exit, `?` early-return, or a panic unwind inside a `spawn_blocking`
/// closure the guard was moved into. This is what stops one inference panic
/// from permanently stranding an item.
///
/// The item and its permit only ever leave the pool together, inside this
/// guard: a caller that keeps the item for longer (a streaming engine) keeps
/// the guard, and reaches the item through it (`Deref`). A permit freed
/// without its item would let a later checkout find nothing to take.
pub struct PooledSlot<T> {
    pool: Arc<SessionPool<T>>,
    item: Option<T>,
    /// Held for as long as the item is out; released on drop, after the
    /// item is back.
    _permit: OwnedSemaphorePermit,
}

impl<T> std::fmt::Debug for PooledSlot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledSlot")
            .field("pool", &self.pool.label)
            .field("held", &self.item.is_some())
            .finish_non_exhaustive()
    }
}

impl<T> PooledSlot<T> {
    /// Borrow the item mutably for in-place work.
    pub fn get_mut(&mut self) -> &mut T {
        self.item.as_mut().expect("item present in guard")
    }
}

impl<T> std::ops::Deref for PooledSlot<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.item.as_ref().expect("item present in guard")
    }
}

impl<T> std::ops::DerefMut for PooledSlot<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.get_mut()
    }
}

impl<T> Drop for PooledSlot<T> {
    fn drop(&mut self) {
        // The item goes back before the permit is released (fields drop
        // after this body), so a waiter the permit lets through finds it.
        if let Some(item) = self.item.take() {
            self.pool.free.lock().push(item);
        }
    }
}

#[cfg(test)]
mod tests;
