use super::*;
use std::io::Cursor;

/// Build a minimal NPZ file in memory for testing.
fn build_test_npz(voices: &[(&str, usize, usize)]) -> Vec<u8> {
    use std::io::Write;

    let mut zip_writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for &(name, rows, cols) in voices {
        let npy_data = build_npy_f32(rows, cols);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip_writer
            .start_file(format!("{name}.npy"), options)
            .unwrap();
        zip_writer.write_all(&npy_data).unwrap();
    }
    zip_writer.finish().unwrap().into_inner()
}

/// Build a minimal NPY file with sequential f32 values.
fn build_npy_f32(rows: usize, cols: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    // NPY magic + version
    buf.extend_from_slice(b"\x93NUMPY");
    buf.push(1); // major
    buf.push(0); // minor

    let header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({rows}, {cols}), }}");
    // Pad header to 64-byte alignment (magic 10 + header_len 2 + header)
    let total = 10 + header.len();
    let padded_len = total.div_ceil(64) * 64 - 10;
    let mut header_bytes = header.into_bytes();
    header_bytes.resize(padded_len, b' ');
    *header_bytes.last_mut().unwrap() = b'\n';

    let header_len = header_bytes.len() as u16;
    buf.extend_from_slice(&header_len.to_le_bytes());
    buf.extend_from_slice(&header_bytes);

    // Data: sequential values for easy verification
    for i in 0..(rows * cols) {
        buf.extend_from_slice(&(i as f32).to_le_bytes());
    }
    buf
}

#[test]
fn load_and_get_style() {
    let npz = build_test_npz(&[("af_heart", 8, 4), ("am_adam", 8, 4)]);
    let voices = KokoroVoices::load_from_reader(Cursor::new(npz)).unwrap();

    assert_eq!(voices.list_voices(), vec!["af_heart", "am_adam"]);
    assert_eq!(voices.style_dim(), 4);

    // Row 3 of af_heart: values [12.0, 13.0, 14.0, 15.0]
    let style = voices.get_style("af_heart", 3).unwrap();
    assert_eq!(style, &[12.0, 13.0, 14.0, 15.0]);
}

#[test]
fn get_style_clamps_to_max_row() {
    let npz = build_test_npz(&[("voice", 4, 2)]);
    let voices = KokoroVoices::load_from_reader(Cursor::new(npz)).unwrap();

    // num_tokens = 100 but only 4 rows -> clamp to row 3
    let style = voices.get_style("voice", 100).unwrap();
    assert_eq!(style, &[6.0, 7.0]); // row 3 of 4x2
}

#[test]
fn unknown_voice_returns_error() {
    let npz = build_test_npz(&[("voice", 2, 2)]);
    let voices = KokoroVoices::load_from_reader(Cursor::new(npz)).unwrap();

    let err = voices.get_style("nonexistent", 0).unwrap_err();
    assert!(err.to_string().contains("nonexistent"));
}

#[test]
fn empty_npz_returns_error() {
    let npz = build_test_npz(&[]);
    let err = KokoroVoices::load_from_reader(Cursor::new(npz)).unwrap_err();
    assert!(err.to_string().contains("no voice entries"));
}

/// A voice with no rows or no columns is refused at load: its data length
/// matches its shape (zero), and a style lookup would read past it.
#[test]
fn an_empty_voice_matrix_is_refused_at_load() {
    for (rows, cols) in [(0, 4), (8, 0)] {
        let npz = build_test_npz(&[("empty", rows, cols)]);
        let err = KokoroVoices::load_from_reader(Cursor::new(npz))
            .err()
            .unwrap_or_else(|| panic!("a {rows}x{cols} voice loaded"));
        assert!(matches!(err, TtsError::ModelLoadFailed(_)), "{err:?}");
    }
}
