use super::*;

#[test]
fn language_for_known_prefixes() {
    assert_eq!(language_for_voice("af_heart"), "en-us");
    assert_eq!(language_for_voice("bf_emma"), "en-gb");
    assert_eq!(language_for_voice("jf_alpha"), "ja");
    assert_eq!(language_for_voice("zf_xiaobei"), "cmn");
}

#[test]
fn language_for_unknown_prefix_defaults_to_en_us() {
    assert_eq!(language_for_voice("xx_unknown"), "en-us");
    assert_eq!(language_for_voice(""), "en-us");
}

#[test]
fn resolve_params_uses_defaults() {
    // We can't construct a full KokoroTts without model files, but we can
    // test the language_for_voice logic directly since resolve_params
    // delegates to it.
    let lang = language_for_voice("am_michael");
    assert_eq!(lang, "en-us");
}

/// With no option of its own, Kokoro follows `VP_ORT_EP` as every other
/// local model does; its own option, when set, decides.
#[test]
fn the_device_follows_vp_ort_ep_unless_the_option_is_set() {
    let from_env = Device::resolve(None, Some("cuda"));
    assert_eq!(from_env.target, ExecutionTarget::Cuda);
    assert_eq!(from_env.setting, EXECUTION_PROVIDER_VAR);
    assert_eq!(Device::resolve(None, None).target, ExecutionTarget::Cpu);
    let from_option = Device::resolve(Some("cpu"), Some("cuda"));
    assert_eq!(from_option.target, ExecutionTarget::Cpu);
    let from_option = Device::resolve(Some("CUDA"), None);
    assert_eq!(from_option.target, ExecutionTarget::Cuda);
    assert_eq!(from_option.setting, DEVICE_OPTION);
}

/// The device report is where the shared opener runs a session asked for
/// on that target, worded as the shared report when `VP_ORT_EP` chose it.
#[test]
fn the_device_report_is_the_openers() {
    for target in [ExecutionTarget::Cpu, ExecutionTarget::Cuda] {
        let by_env = Device {
            target,
            setting: EXECUTION_PROVIDER_VAR,
        };
        assert_eq!(by_env.report(), target.device_report());
        let by_option = Device {
            target,
            setting: DEVICE_OPTION,
        };
        assert_eq!(by_option.report().0, target.effective().0.as_str());
    }
    let (_, detail) = Device::resolve(Some("cuda"), None).report();
    assert!(
        detail
            .as_deref()
            .is_some_and(|d| d.starts_with("execution_provider=cuda")),
        "{detail:?}"
    );
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::AudioFrame>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        for _ in 0..2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            tx.send(voice_pipes_sdk::AudioFrame {
                samples: vec![0; 320],
                sample_rate: 24_000,
                channels: 1,
            })
            .await
            .unwrap();
        }
        Ok(())
    };

    let res = voice_pipes_sdk::testing::assert_tts_metrics_conformance(
        run,
        std::time::Duration::from_millis(15),
    )
    .await;
    assert!(res.is_ok(), "{res:?}");
}

