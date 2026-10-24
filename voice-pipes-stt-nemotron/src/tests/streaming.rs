use super::*;

/// A state tensor's size is the product of its dimensions; a negative
/// dimension or a product that does not fit is a broken export, refused
/// with the state's name.
#[test]
fn a_state_shape_is_counted_or_refused() {
    assert_eq!(
        element_count("cache_a", &[1, 24, 70, 1024]).ok(),
        Some(1_720_320)
    );
    assert_eq!(element_count("cache_b", &[]).ok(), Some(1));
    for bad in [&[1, -2, 3][..], &[i64::MAX, i64::MAX][..]] {
        let err = element_count("cache_c", bad).expect_err("refused");
        assert!(err.to_string().contains("cache_c"), "{err}");
    }
}

/// `threads = 0` (or a negative count) leaves the choice to ONNX Runtime,
/// as it did before the session opener was shared and as it does for every
/// other local model; a count is passed through.
#[test]
fn zero_threads_leaves_the_count_to_the_runtime() {
    assert_eq!(session_threads(0), 0);
    assert_eq!(session_threads(-3), 0);
    assert_eq!(session_threads(4), 4);
}

/// A channel-first `[1, dims, width]` tensor whose element at channel `d`,
/// frame `t` is `100 * d + t`.
fn channel_first(dims: usize, width: usize) -> Vec<f32> {
    (0..dims)
        .flat_map(|d| (0..width).map(move |t| (100 * d + t) as f32))
        .collect()
}

/// The regression: a padded final chunk reports fewer valid frames than the
/// tensor is wide, and the frames were read with the valid length as the
/// row stride, landing on the wrong elements. Frame `t` is the column at
/// `t` whatever the valid length.
#[test]
fn a_padded_chunk_reads_each_frame_at_its_own_column() {
    // An embedding is wider than a chunk is long, as in every export.
    let (dims, width, valid) = (16, 6, 3);
    let tensor = channel_first(dims, width);
    let layout = FrameLayout::of(&[1, dims as i64, width as i64], valid).expect("layout");
    for t in 0..valid {
        let expected: Vec<f32> = (0..dims).map(|d| (100 * d + t) as f32).collect();
        assert_eq!(
            layout.frame(&tensor, t).expect("a whole frame"),
            expected,
            "frame {t}"
        );
    }
}

/// Time-first and channel-first exports both read right, full or padded,
/// including an embedding as wide as the chunk is long.
#[test]
fn the_time_axis_is_found_in_either_layout() {
    // Time first, [1, width, dims].
    let time_first: Vec<f32> = (0..5)
        .flat_map(|t| (0..8).map(move |d| (100 * d + t) as f32))
        .collect();
    for valid in [5, 3] {
        let layout = FrameLayout::of(&[1, 5, 8], valid).expect("layout");
        assert_eq!(
            layout.frame(&time_first, 1).expect("a whole frame"),
            (0..8).map(|d| (100 * d + 1) as f32).collect::<Vec<_>>()
        );
    }
    // Channel first, full chunk.
    let full = channel_first(8, 5);
    let layout = FrameLayout::of(&[1, 8, 5], 5).expect("layout");
    assert_eq!(
        layout.frame(&full, 4).expect("a whole frame"),
        (0..8).map(|d| (100 * d + 4) as f32).collect::<Vec<_>>()
    );
    // No frames, or more than fit, is no layout.
    assert_eq!(FrameLayout::of(&[1, 8, 5], 0), None);
    assert_eq!(FrameLayout::of(&[1, 4, 3], 9), None);
}

/// An export whose chunk is zero would advance no window and hang the
/// decoder on the blocking pool; it is refused at load, as is a size past
/// any export.
#[test]
fn an_export_with_an_empty_or_huge_window_is_refused() {
    let metadata = |left: usize, chunk: usize, right: usize| {
        format!(r#"{{"streaming_config": {{"left": {left}, "chunk": {chunk}, "right": {right}}}}}"#)
    };
    assert!(parse_metadata(&metadata(70, 1, 0)).is_ok());
    for (left, chunk, right) in [(70, 0, 0), (70, 1, usize::MAX), (usize::MAX, 1, 0)] {
        assert!(
            parse_metadata(&metadata(left, chunk, right)).is_err(),
            "{left}/{chunk}/{right}"
        );
    }
}
