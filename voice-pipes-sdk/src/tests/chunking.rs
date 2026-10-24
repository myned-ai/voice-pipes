use super::*;

/// A transform that carries its state across chunks passes, and the whole
/// input's output comes back.
#[test]
fn a_transform_that_carries_state_across_chunks_passes() {
    let sum_of_pairs = |chunks: &[&[u8]]| {
        let (mut out, mut pending) = (Vec::new(), None);
        for chunk in chunks {
            for &b in *chunk {
                match pending.take() {
                    Some(a) => out.push(u16::from(a) + u16::from(b)),
                    None => pending = Some(b),
                }
            }
        }
        out
    };
    assert_eq!(chunking_invariant(&[1, 2, 3, 4, 5], sum_of_pairs), [3, 7]);
}

/// A transform that forgets its state at a chunk edge is caught, with the
/// chunk size that shows it.
#[test]
#[should_panic(expected = "fed 1 at a time")]
fn a_transform_that_drops_state_at_a_chunk_edge_fails() {
    chunking_invariant(&[1u8, 2, 3, 4], |chunks| {
        chunks
            .iter()
            .flat_map(|c| {
                c.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|[a, b]| u16::from(*a) + u16::from(*b))
            })
            .collect::<Vec<_>>()
    });
}

/// An empty input is run once, whole.
#[test]
fn an_empty_input_runs_once() {
    let mut runs = 0;
    chunking_invariant(&[] as &[u8], |chunks| {
        runs += 1;
        chunks.len()
    });
    assert_eq!(runs, 1);
}
