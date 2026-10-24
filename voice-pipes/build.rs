//! Build script -- compiles `.proto` files when any gRPC feature is enabled.

fn main() {
    #[cfg(any(
        feature = "stt-grpc",
        feature = "tts-grpc",
        feature = "text-grpc",
        feature = "component-server",
        feature = "post-grpc",
    ))]
    compile_protos();
}

#[cfg(any(
    feature = "stt-grpc",
    feature = "tts-grpc",
    feature = "text-grpc",
    feature = "component-server",
    feature = "post-grpc",
))]
fn compile_protos() {
    let mut protos = Vec::new();

    if cfg!(any(feature = "stt-grpc", feature = "component-server")) {
        protos.push("proto/stt.proto");
    }
    if cfg!(any(feature = "tts-grpc", feature = "component-server")) {
        protos.push("proto/tts.proto");
    }
    if cfg!(any(feature = "text-grpc", feature = "component-server")) {
        protos.push("proto/text_processor.proto");
        // Agent-session link
        // rides the text-grpc feature: the gateway processor that
        // consumes it is a TextProcessor.
        protos.push("proto/agent.proto");
    }
    if cfg!(feature = "post-grpc") {
        // Generic audio post-processor.
        // Standalone: no shared message imports.
        protos.push("proto/audio_post_processor.proto");
    }
    // ParamService is shared across all gRPC features so the orchestrator
    // can forward `PATCH /debug/params` to any remote component server.
    if cfg!(any(
        feature = "stt-grpc",
        feature = "tts-grpc",
        feature = "text-grpc",
        feature = "component-server",
        feature = "post-grpc",
    )) {
        protos.push("proto/params.proto");
    }

    if !protos.is_empty() {
        // Vendored protoc unless the environment provides one.
        if std::env::var_os("PROTOC").is_none() {
            let protoc = protoc_bin_vendored::protoc_bin_path()
                .expect("no vendored protoc for this platform");
            // SAFETY: build scripts are single-threaded at this point;
            // no concurrent env access.
            unsafe { std::env::set_var("PROTOC", protoc) };
        }
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .compile_protos(&protos, &["proto"])
            .expect("failed to compile proto files");
    }
}
