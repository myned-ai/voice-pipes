"""Download the default VibeVoice reference voice prompt .pt files from GitHub."""

import os
import urllib.request

VOICES = [
    "de-Spk0_man.pt",
    "de-Spk1_woman.pt",
    "en-Carter_man.pt",
    "en-Davis_man.pt",
    "en-Emma_woman.pt",
    "en-Frank_man.pt",
    "en-Grace_woman.pt",
    "en-Mike_man.pt",
    "fr-Spk0_man.pt",
    "fr-Spk1_woman.pt",
    "in-Samuel_man.pt",
    "it-Spk0_woman.pt",
    "it-Spk1_man.pt",
    "jp-Spk0_man.pt",
    "jp-Spk1_woman.pt",
    "kr-Spk0_woman.pt",
    "kr-Spk1_man.pt",
    "nl-Spk0_man.pt",
    "nl-Spk1_woman.pt",
    "pl-Spk0_man.pt",
    "pl-Spk1_woman.pt",
    "pt-Spk0_woman.pt",
    "pt-Spk1_man.pt",
    "sp-Spk0_woman.pt",
    "sp-Spk1_man.pt",
]

BASE_URL = "https://raw.githubusercontent.com/microsoft/VibeVoice/main/demo/voices/streaming_model/"
_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..'))
_VP = os.path.join(_ROOT, 'voice-pipes') if os.path.isdir(os.path.join(_ROOT, 'voice-pipes')) else _ROOT
TARGET_DIR = os.path.join(
    _VP,
    "VibeVoice",
    "demo",
    "voices",
    "streaming_model",
)


def main():
    os.makedirs(TARGET_DIR, exist_ok=True)
    print(f"Downloading default VibeVoice speaker voice files ({len(VOICES)} files, ~95 MB total)")
    print(f"Target directory: {os.path.abspath(TARGET_DIR)}")

    for i, voice in enumerate(VOICES, 1):
        url = BASE_URL + voice
        target_path = os.path.join(TARGET_DIR, voice)

        if os.path.exists(target_path):
            print(f"[{i}/{len(VOICES)}] {voice} already exists, skipping.")
            continue

        print(f"[{i}/{len(VOICES)}] Downloading {voice}...", end="", flush=True)
        try:
            urllib.request.urlretrieve(url, target_path)
            print(" Done!")
        except Exception as e:
            print(f" Failed! Error: {e}")


if __name__ == "__main__":
    main()
