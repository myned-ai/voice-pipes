use super::*;

/// The symmetric window touches zero at both ends; the periodic one only at
/// the start, as if one sample longer.
#[test]
fn the_two_hann_windows_differ_where_the_references_do() {
    let s = hann(5, Hann::Symmetric);
    assert_eq!(s.len(), 5);
    assert!(s[0].abs() < 1e-7 && s[4].abs() < 1e-7, "{s:?}");
    assert!((s[2] - 1.0).abs() < 1e-7, "{s:?}");
    let p = hann(4, Hann::Periodic);
    assert!(p[0].abs() < 1e-7, "{p:?}");
    assert!((p[2] - 1.0).abs() < 1e-7, "{p:?}");
    assert!(p[3] > 0.0, "{p:?}");
    assert_eq!(hann(1, Hann::Symmetric), [1.0]);
    assert!(hann(0, Hann::Symmetric).is_empty());
}

#[test]
fn a_short_window_is_centred_in_its_frame() {
    assert_eq!(centred(&[1.0, 2.0], 6), [0.0, 0.0, 1.0, 2.0, 0.0, 0.0]);
}

/// Each band is a triangle over the bins, non-negative, peaking once; the
/// bands cover the range in order.
#[test]
fn the_filterbank_has_one_triangle_per_band_in_order() {
    let fb = slaney_filterbank(16_000, 512, 128, 0.0, 8_000.0);
    assert_eq!(fb.len(), 128);
    assert!(fb.iter().all(|row| row.len() == 257));
    let peaks: Vec<usize> = fb
        .iter()
        .map(|row| {
            assert!(row.iter().all(|w| *w >= 0.0));
            row.iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(k, _)| k)
                .unwrap()
        })
        .collect();
    assert!(peaks.windows(2).all(|w| w[0] <= w[1]), "{peaks:?}");
}

/// A sinusoid at an exact bin's frequency puts its power in that bin; the
/// frame count follows the hop.
#[test]
fn a_tone_lands_in_its_bin() {
    let n_fft = 64;
    let tone: Vec<f32> = (0..256)
        .map(|i| (2.0 * std::f32::consts::PI * 8.0 * i as f32 / n_fft as f32).sin())
        .collect();
    let spec = PowerSpectrogram::new(vec![1.0; n_fft], 32);
    assert_eq!(spec.frames(tone.len()), 7);
    assert_eq!(spec.frames(n_fft - 1), 0);
    let mut seen = 0;
    spec.for_each(&tone, 7, |_, power| {
        let peak = power
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, _)| k);
        assert_eq!(peak, Some(8));
        seen += 1;
    });
    assert_eq!(seen, 7);
}

#[test]
fn a_band_energy_is_its_row_dotted_with_the_spectrum() {
    let fb = vec![vec![1.0, 0.0, 2.0], vec![0.5, 0.5, 0.0]];
    let e: Vec<f32> = mel_energies(&fb, &[1.0, 2.0, 3.0]).collect();
    assert_eq!(e, [7.0, 1.5]);
}
