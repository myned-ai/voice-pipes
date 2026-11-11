use super::*;

#[test]
fn output_names_follow_the_export_convention() {
    assert_eq!(cache_output_name("k_cache_0"), "k_cache_out_0");
    assert_eq!(cache_output_name("conv1d_cache_23"), "conv1d_cache_out_23");
    assert_eq!(cache_output_name("conv2d_cache_2"), "conv2d_cache_out_2");
}

#[test]
fn the_320ms_schedule_is_the_one_wp_a_measured() {
    let s = Schedule::for_lookahead(3);
    assert_eq!((s.enc_frames, s.mel_first, s.mel_steady), (4, 25, 32));
    assert_eq!(s.samples_first, 24 * 160 + 200);
    assert_eq!(s.samples_steady, 32 * 160 + 400);
}
