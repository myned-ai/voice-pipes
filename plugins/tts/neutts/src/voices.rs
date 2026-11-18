//! Built-in default reference voices for NeuTTS-Air.

/// Default reference text (speaker "Jo", 3-second reference clip).
#[allow(dead_code, reason = "reference speaker transcript")]
pub const JO_REFERENCE_TEXT: &str = "So I just tried Neuphonic and I’m genuinely impressed.";

/// Pre-computed espeak-ng IPA phonemes for speaker "Jo" (3-second clip).
pub const JO_REFERENCE_PHONEMES: &str =
    "sˌoʊ aɪ dʒˈʌst tɹˈaɪd nuːfˈɑːnɪk ænd aɪm dʒˈɛnjuːɪnli ɪmpɹˈɛst.";

/// Pre-encoded `NeuCodec` reference acoustic tokens for speaker "Jo" (142 tokens = 2.84s).
///
/// Sliced in the exact silence gap (tokens 139..143, RMS < 0.002) immediately after
/// "So I just tried Neuphonic and I’m genuinely impressed.", before "It's super responsive..."
/// begins at token 144 in upstream `samples/jo.pt`.
pub const JO_REFERENCE_CODES: &[i32] = &[
    18578, 55378, 55300, 39208, 51497, 35404, 680, 3788, 37839, 13516, 12750, 2952, 59910, 63022,
    54058, 54093, 41753, 62218, 55850, 2172, 39210, 54633, 50534, 36909, 33033, 52165, 34696,
    19420, 57482, 6348, 8262, 12444, 4700, 26087, 59531, 62950, 56151, 57798, 47976, 48805, 64011,
    54298, 52832, 51537, 51239, 61145, 42469, 52380, 38622, 52941, 51166, 1740, 599, 6855, 4742,
    684, 33563, 17212, 17194, 43759, 50346, 61510, 55677, 54774, 50326, 53597, 53372, 54045, 53356,
    53597, 17574, 55505, 50656, 45496, 17181, 63170, 649, 45708, 29263, 46093, 12749, 48590, 48090,
    48859, 47810, 63959, 65259, 63597, 60971, 59455, 10538, 38455, 18081, 53689, 614, 22506, 12147,
    47862, 4467, 46762, 14754, 40697, 16097, 39573, 6954, 48710, 43958, 40875, 40806, 14929, 46641,
    5811, 18231, 22526, 58681, 58721, 54004, 17164, 51918, 39899, 23039, 51947, 51446, 56450,
    63578, 47691, 63838, 61078, 3520, 47876, 45768, 61761, 4280, 20742, 13428, 45651, 63570, 58631,
    55332, 50480, 36000, 50322,
];
