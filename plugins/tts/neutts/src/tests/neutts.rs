use std::time::Duration;
use tokio_util::sync::CancellationToken;
use voice_pipes_sdk::{AudioFrame, TtsConfig};

#[test]
fn factory_metadata() {
    let f = super::factory();
    assert_eq!(f.name(), "neutts");
    assert!(f.is_local());
    assert!(f.aliases().contains(&"neutts-air"));
}

#[test]
fn config_validation_requires_model_and_vocoder() {
    let f = super::factory();
    let empty_cfg: TtsConfig = toml::from_str("provider = \"neutts\"").unwrap();
    let errs = f.validate(&empty_cfg);
    assert_eq!(errs.len(), 2);
}

#[tokio::test]
async fn metrics_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<AudioFrame>, _cancel: CancellationToken| async move {
        tokio::time::sleep(Duration::from_millis(15)).await;
        let _ = tx
            .send(AudioFrame {
                samples: vec![0i16; 1600],
                sample_rate: 16_000,
                channels: 1,
            })
            .await;
        Ok(())
    };

    let res = voice_pipes_sdk::testing::assert_tts_metrics_conformance(
        run,
        Duration::from_millis(15),
    )
    .await;
    assert!(res.is_ok(), "{res:?}");
}
