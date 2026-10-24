# Quickstart with Docker

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

The fastest way to see Voice Pipes work: one command starts an orchestrator, a
worker and the admin console, downloads the models you pick, and needs
nothing on the host but Docker.

## Prerequisites

- Docker Desktop, or Docker Engine with the Compose plugin.
- About 2 GB of free disk for the default local models; more if you pick a
  larger LLM.
- Optional: an NVIDIA GPU with the NVIDIA Container Toolkit. The launcher
  probes for GPU pass-through and only the VibeVoice and CUDA paths need it.

## Run the launcher

```sh
./launch-stack.sh     # Linux / macOS / WSL
.\launch-stack.ps1    # Windows PowerShell
```

The launcher:

1. checks that the Docker daemon is running and probes GPU support;
2. runs the interactive **configuration wizard** in a container;
3. writes the pipeline and worker configuration it produced;
4. downloads the model files your choices need;
5. starts the orchestrator, the worker (CPU or GPU image, chosen from the
   probe) and the admin console.

**First-run tips.** Pick the local defaults: Parakeet for STT, a local GGUF
LLM such as Gemma or SmolLM2-360M, Kokoro or Piper for TTS. They need no API keys
and run on CPU. The first start downloads a few hundred megabytes to a couple
of gigabytes of model files, so expect a few minutes; later starts are fast.
Downloaded models keep their upstream licenses; Gemma is under Google's Gemma
Terms and is gated, so its download needs a Hugging Face token (`HF_TOKEN`).

When it is up:

| Service | Address |
|---|---|
| Admin console | http://localhost:3000 |
| Orchestrator API | http://localhost:9090 |
| Worker WebSocket | ws://localhost:8800/ws |

The console asks for a sign-in token on first load. The launcher generates
one into `voice-pipes-admin/.env` (`ADMIN_UI_TOKEN`) and prints it; paste it
into the form. It also generates the three engine tokens
(`VP_CLIENT_TOKEN`, `VP_ADMIN_TOKEN`, `VP_INTERNAL_TOKEN`) into
`voice-pipes/.env`; the shipped configs refuse to start without them. See
[Admin console › Authentication](../guides/admin-console.md#authentication)
and [Deployment › Authentication](../guides/deployment.md#authentication).

Press `Ctrl+C` in the launcher terminal to stop the whole stack.

## Your first conversation

1. Open the admin console and go to **Playground**.
2. Click **Connect** and grant microphone access.
3. Say something. The agent transcribes, thinks and answers out loud; the
   transcript reveals word by word in sync with the audio.
4. **Interrupt it mid-sentence.** It stops immediately. That is the barge-in
   monitor.
5. Open **Sessions**, click the conversation you just had, and look at the
   per-turn breakdown: how many milliseconds VAD, STT, the LLM and TTS each
   spent on every reply.

## Cloud providers and API keys

Cloud providers read keys from the environment by name:

| Variable | Used by |
|---|---|
| `OPENAI_API_KEY` | OpenAI Realtime, OpenAI TTS, any OpenAI-compatible LLM |
| `GEMINI_API_KEY` | Gemini Live (speech-to-speech) |
| `GOOGLE_API_KEY` | Gemini TTS |
| `ELEVENLABS_API_KEY`, `ELEVENLABS_AGENT_ID` | ElevenLabs TTS and ElevenLabs Agents |
| `ASSEMBLYAI_API_KEY` | AssemblyAI streaming STT |
| `GOOGLE_CLOUD_PROJECT`, `GOOGLE_APPLICATION_CREDENTIALS` | Google Cloud STT (Chirp 3); no API key, uses Application Default Credentials |
| `HF_TOKEN` | Downloading gated models |

None is required for the local-only stack. Copy
[`voice-pipes/.env.example`](../../voice-pipes/.env.example) to
`voice-pipes/.env` to set them per checkout; the launcher loads that file and
passes keys to containers by name, so values never appear in the compose file.
Values already set in your shell win. The wizard writes the file for you when
you type a key at a prompt.

## Change your mind

Run the launcher again to re-run the wizard, or edit the generated
configuration under `voice-pipes/deploy/docker/config/` and start the stack
on it without the questions: `./launch-stack.sh --skip-wizard` or
`.\launch-stack.ps1 -SkipWizard`. `rebuild-stack.ps1` rebuilds the images. The configuration format is described in
[Configuration](../guides/configuration.md).

## Manual compose stack

If you prefer explicit control, the compose file at
`voice-pipes/deploy/docker/docker-compose.yml` starts an orchestrator on 9090
and four CPU workers: the wizard's `worker-custom` on 8800 (debug API on
8085), `worker-english-corpus` on 8803, and the two English recogniser
comparison workers, `worker-english-moonshine` on 8807 and
`worker-english-nemotron35` on 8810. Everything else is behind a profile:
`gpu` holds `worker-custom-gpu`, the CUDA build of `worker-custom` on the
same ports (start it by name instead of `worker-custom`), and `worker-gpu`;
`ab` adds an A/B pair of workers on one pipeline, `bench-s2s` the
speech-to-speech benchmark workers, `nemotron` another recogniser worker,
and `llama`, `pocket` and `vibevoice` the model-server sidecars (their host
ports are loopback-only). It expects
model files under `voice-pipes/models/` and an OpenAI-compatible LLM at
`http://host.docker.internal:8081/v1` by default.

```sh
cd voice-pipes
docker compose -f deploy/docker/docker-compose.yml up --build -d
docker compose -f deploy/docker/docker-compose.yml --profile gpu up --build -d orchestrator worker-custom-gpu   # the GPU worker instead of the CPU one
docker compose -f deploy/docker/docker-compose.yml down
```

The launcher is the supported path; the manual stack is for people who want
to wire their own services. See [Deployment](../guides/deployment.md).

## Next

- [Your first agent](first-agent.md) — change the LLM, the voice and the prompt.
- [Quickstart from source](quickstart-source.md) — run the CLI natively.
- [Concepts](concepts.md) — the vocabulary.
