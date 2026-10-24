//! The chunking invariant: what a stream transform produces does not depend
//! on how its input was cut.

use std::fmt::Debug;

/// Check that a stream transform's output does not depend on how its input
/// is chunked, and return that output.
///
/// `run` builds a fresh transform, feeds it the chunks it is given in
/// order, finishes it, and returns everything it produced. It is called
/// once with the whole input as one chunk, then once for every chunk size
/// from 1 to the input's length (the last chunk of each shorter where the
/// size does not divide it); each output must equal the first. One element
/// at a time is where boundary bugs live: a mark split from what follows
/// it, half a sample, a header cut in two. Keep `input` short (a few
/// hundred elements): the transform runs once per chunk size.
///
/// # Panics
///
/// When a chunking produces a different output, naming its chunk size.
pub fn chunking_invariant<T, O, F>(input: &[T], mut run: F) -> O
where
    O: PartialEq + Debug,
    F: FnMut(&[&[T]]) -> O,
{
    let whole = run(&[input]);
    for size in 1..=input.len() {
        let chunks: Vec<&[T]> = input.chunks(size).collect();
        let out = run(&chunks);
        assert_eq!(
            out, whole,
            "fed {size} at a time, the output differs from the whole input's"
        );
    }
    whole
}

#[cfg(test)]
#[path = "../tests/chunking.rs"]
mod tests;
