//! Tests for the shared inference pool.

use std::sync::Arc;
use std::time::Duration;

use super::{EmptyPool, PoolExhausted, PooledSlot, SessionPool, argmax};

fn pool(n: usize) -> Arc<SessionPool<usize>> {
    Arc::new(SessionPool::new((0..n).collect(), "test engines").expect("a non-empty pool"))
}

/// Empty logits have no largest value. Answering index 0 made a broken
/// model output decode as token 0; there is now no answer to mistake.
#[test]
fn argmax_of_nothing_is_no_index() {
    assert_eq!(argmax(&[]), None);
    assert_eq!(argmax(&[f32::NAN, f32::NAN]), None);
    assert_eq!(argmax(&[0.1, 0.9, 0.3]), Some(1));
    assert_eq!(argmax(&[0.5, 0.5]), Some(0));
    assert_eq!(argmax(&[f32::NAN, -1.0]), Some(1));
    assert_eq!(argmax(&[f32::NEG_INFINITY]), Some(0));
}

/// A pool of nothing would make every checkout wait out its deadline and
/// report "exhausted"; it is refused when built instead.
#[test]
fn an_empty_pool_is_refused() {
    let err = SessionPool::<usize>::new(Vec::new(), "test engines").unwrap_err();
    assert_eq!(
        err,
        EmptyPool {
            label: "test engines"
        }
    );
    assert!(err.to_string().contains("test engines"), "{err}");
}

/// A sub-second wait printed as "waited 0 s".
#[test]
fn the_error_states_the_wait_in_milliseconds() {
    let text = PoolExhausted {
        label: "Piper engines",
        size: 2,
        waited: Duration::from_millis(250),
    }
    .to_string();
    assert!(text.contains("waited 250 ms"), "{text}");
}

#[tokio::test]
async fn checkout_lends_an_item_and_drop_returns_it() {
    let pool = pool(2);
    assert_eq!(pool.available(), 2);

    let mut slot = pool.checkout().await.expect("free item available");
    assert_eq!(pool.available(), 1);
    *slot.get_mut() += 100;
    drop(slot);

    assert_eq!(pool.available(), 2);
}

#[tokio::test]
async fn a_full_pool_rejects_a_non_blocking_checkout() {
    let pool = pool(2);
    let _a = pool.try_checkout().expect("first checkout");
    let _b = pool.try_checkout().expect("second checkout");
    assert!(
        pool.try_checkout().is_none(),
        "a full pool must reject a third checkout"
    );
}

/// The regression this pool exists for: before it, an exhausted pool awaited
/// the semaphore with no deadline, so a saturated worker hung the session
/// instead of answering. The wait must end in a typed error.
#[tokio::test(start_paused = true)]
async fn exhausted_pool_reports_instead_of_waiting_forever() {
    let pool = pool(1);
    let _held = pool.try_checkout().expect("first checkout succeeds");
    assert_eq!(pool.available(), 0);

    let err = pool
        .checkout_within(Duration::from_secs(5))
        .await
        .expect_err("an exhausted pool must not block indefinitely");

    assert_eq!(
        err,
        PoolExhausted {
            label: "test engines",
            size: 1,
            waited: Duration::from_secs(5),
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_slot_freed_before_the_deadline_is_handed_over() {
    let pool = pool(1);
    let held = pool.try_checkout().expect("first checkout succeeds");

    let waiter = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.checkout_within(Duration::from_secs(5)).await })
    };

    // Free the item well inside the deadline.
    tokio::time::sleep(Duration::from_secs(1)).await;
    drop(held);

    let slot = waiter
        .await
        .expect("waiter task joins")
        .expect("the freed item is handed to the waiter");
    assert_eq!(pool.available(), 0);
    drop(slot);
    assert_eq!(pool.available(), 1);
}

/// A panic inside the blocking closure the guard was moved into must not
/// strand the item: that is what turns one bad inference into a permanently
/// shrunken pool.
#[tokio::test]
async fn a_panic_while_holding_the_guard_returns_the_item() {
    let pool = pool(1);

    let slot = pool.checkout().await.expect("free item available");
    let joined = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        panic!("inference blew up");
    })
    .await;
    assert!(joined.is_err(), "the blocking task panicked");

    assert_eq!(pool.available(), 1, "the item came back to the pool");
    pool.checkout()
        .await
        .expect("the pool is usable after a panic");
}

#[test]
fn the_error_names_the_knob_to_turn() {
    let text = PoolExhausted {
        label: "Piper engines",
        size: 2,
        waited: Duration::from_secs(5),
    }
    .to_string();

    assert!(text.contains("all 2 Piper engines are in use"), "{text}");
    assert!(text.contains("pool_size"), "{text}");
}

/// The regression: a streaming engine took the item out of its slot and
/// the permit separately, and a failed build dropped the item and freed
/// the permit, so the next checkout found a permit and no item and
/// panicked. The item now travels in its slot: a constructor that fails
/// drops the slot, which returns both.
#[tokio::test]
async fn a_failed_build_with_a_checked_out_item_leaves_the_pool_whole() {
    struct Engine {
        _slot: PooledSlot<usize>,
    }
    fn build(slot: PooledSlot<usize>, ok: bool) -> Result<Engine, &'static str> {
        if ok {
            Ok(Engine { _slot: slot })
        } else {
            Err("build failed")
        }
    }
    let p = pool(1);
    for _ in 0..3 {
        let slot = p.checkout().await.expect("the pool's one item");
        assert!(build(slot, false).is_err());
        assert_eq!(p.available(), 1);
    }
    let engine = build(p.checkout().await.expect("item back"), true).unwrap();
    assert_eq!(p.available(), 0);
    drop(engine);
    assert_eq!(p.available(), 1);
}

#[test]
fn checkout_blocking_waits_for_returned_slot_and_times_out_when_exhausted() {
    let p = pool(1);
    let held = p
        .checkout_blocking_within(Duration::from_millis(50))
        .expect("initial blocking checkout");
    assert_eq!(
        p.checkout_blocking_within(Duration::from_millis(20))
            .expect_err("pool is full"),
        PoolExhausted {
            label: "test engines",
            size: 1,
            waited: Duration::from_millis(20),
        }
    );

    let p2 = Arc::clone(&p);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let b2 = Arc::clone(&barrier);
    let releaser = std::thread::spawn(move || {
        b2.wait();
        drop(held);
    });
    barrier.wait();
    let acquired = p2
        .checkout_blocking_within(Duration::from_millis(500))
        .expect("woken when slot returned");
    releaser.join().expect("releaser thread");
    drop(acquired);
    assert_eq!(p.available(), 1);
}
