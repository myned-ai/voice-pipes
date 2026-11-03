# Getting Support for Voice Pipes

Thanks for using **Voice Pipes**! Here is the fastest way to get help depending on what you need:

## 1. Self-Diagnosis (Fastest)

Before posting a question or bug report, Voice Pipes includes built-in diagnostic tools that pinpoint most issues in seconds:

- **Verify every pipeline component offline:**
  ```sh
  voice-pipes --config <your-config.toml> check
  ```
- **Inspect a live or recorded session:**
  Open the **Admin Console → Sessions**, select the session, and click **"Report for LLM"** to copy a structured latency, health, and anomaly summary (see the [Troubleshooting Guide](../docs/guides/troubleshooting.md) and [Session Diagnostic Guide](../docs/reference/session-diagnostic-guide.md)).

## 2. Where to Ask

| Topic | Where to go |
| :--- | :--- |
| **Questions, configuration help, latency tuning, or hardware/GPU sizing** | [GitHub Discussions — Q&A](https://github.com/myned-ai/voice-pipes/discussions/categories/q-a) |
| **Architecture proposals, RFCs, or VPO-Metrics feedback** | [GitHub Discussions — Ideas](https://github.com/myned-ai/voice-pipes/discussions/categories/ideas) |
| **Sharing a custom plugin (`voice-pipes-sdk`) or voice agent** | [GitHub Discussions — Show and Tell](https://github.com/myned-ai/voice-pipes/discussions/categories/show-and-tell) |
| **Reproducible bugs or concrete feature/provider requests** | [GitHub Issues](https://github.com/myned-ai/voice-pipes/issues/new/choose) |
| **Security vulnerabilities** | **Do not open a public issue.** Follow [SECURITY.md](../SECURITY.md) (`security@myned.ai`). |
| **Commercial licensing (> $5M USD revenue), SLAs, or enterprise support** | See [COMMERCIAL.md](../COMMERCIAL.md) or email [info@myned.ai](mailto:info@myned.ai). |
