"""Download VibeVoice-Realtime-0.5B model from Hugging Face."""

import os
from huggingface_hub import snapshot_download

MODEL_REPO = "microsoft/VibeVoice-Realtime-0.5B"
LOCAL_DIR = os.path.join(os.path.dirname(__file__), "..", "..", "models", "vibevoice")


def main():
    os.makedirs(LOCAL_DIR, exist_ok=True)
    print(f"Downloading {MODEL_REPO} → {os.path.abspath(LOCAL_DIR)}")
    snapshot_download(
        repo_id=MODEL_REPO,
        local_dir=LOCAL_DIR,
        local_dir_use_symlinks=False,
    )
    print(f"\nDone! Model saved to: {os.path.abspath(LOCAL_DIR)}")
    print(f"Start the server with:")
    print(f"  python scripts/vibevoice/server.py --model-path {LOCAL_DIR}")


if __name__ == "__main__":
    main()
