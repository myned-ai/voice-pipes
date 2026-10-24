//! Compiles the trimmed Google Speech v2 proto.

fn main() {
    // Vendored protoc unless the environment provides one.
    if std::env::var_os("PROTOC").is_none() {
        let protoc =
            protoc_bin_vendored::protoc_bin_path().expect("no vendored protoc for this platform");
        // SAFETY: build scripts are single-threaded at this point; no
        // concurrent env access.
        unsafe { std::env::set_var("PROTOC", protoc) };
    }
    // The proto imports `google/protobuf/duration.proto`; the well-known
    // types ship next to the vendored binary and prost-build adds
    // PROTOC_INCLUDE to the import path.
    if std::env::var_os("PROTOC_INCLUDE").is_none()
        && let Ok(include) = protoc_bin_vendored::include_path()
    {
        // SAFETY: see above.
        unsafe { std::env::set_var("PROTOC_INCLUDE", include) };
    }
    println!("cargo:rerun-if-changed=proto/google_speech_v2.proto");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/google_speech_v2.proto"], &["proto"])
        .expect("failed to compile proto/google_speech_v2.proto");
}
