# Brain server — macOS and Linux

Python 3.10 or newer; standard library only. The Brain owns command queues, future LLM/tool integrations, and API credentials. The ESP32 remains the physical sensor/face/audio/motor frontend.

## Start

From the repository root:

    cd server
    python3 -m espbot

Health: http://127.0.0.1:8766/health

Default bind is localhost only. For a Korvo board to access this Mac/Linux service on a trusted LAN:

    export ESPBOT_BIND=0.0.0.0
    export ESPBOT_TOKEN="$(python3 -c 'import secrets;print(secrets.token_urlsafe(32))')"
    python3 -m espbot

Configure the same token and the Mac/Linux LAN URL (for example http://192.168.1.10:8766) in the firmware ESP-IDF menuconfig, under ESP Bot. Device ID defaults to korvo-01.

Security: this v0.1 prototype uses plain HTTP. Use **trusted LAN only**, do not port-forward or expose to the Internet. Production pairing, TLS/mTLS, secrets storage, tool permission rules and streaming auth are future work. Setting ESPBOT_BIND=0.0.0.0 **without** ESPBOT_TOKEN is rejected.

## Control the face

    curl -X POST http://127.0.0.1:8766/v1/commands \
      -H 'Content-Type: application/json' \
      -H "Authorization: Bearer $ESPBOT_TOKEN" \
      -d '{"device_id":"korvo-01","type":"face.set","payload":{"theme":"sakura","expression":"happy","subtitle":"你好！"}}'

A configured ESP32 fetches the next command with:

    GET /v1/commands/next?device_id=korvo-01

Supported fields: theme (one of five cohesive color palettes), expression, subtitle, gaze_x and gaze_y (-100..100). Omitted fields are unchanged. Both eyes always share one theme color.

## Optional OpenAI-compatible LLM

Provider credentials remain on the server. To connect an OpenAI-compatible local LLM endpoint, for example:

    export ESPBOT_LLM_BASE_URL=http://127.0.0.1:11434/v1
    export ESPBOT_LLM_MODEL=your-installed-model
    # export ESPBOT_LLM_API_KEY=only-if-needed
    python3 -m espbot

Then call:

    curl -X POST http://127.0.0.1:8766/v1/chat \
      -H 'Content-Type: application/json' \
      -H "Authorization: Bearer $ESPBOT_TOKEN" \
      -d '{"device_id":"korvo-01","message":"Say hello in one short sentence."}'

When configured, the Brain sends the prompt to POST /chat/completions and queues a subtitle with the returned answer. When not configured, the API returns 503. Nothing is silently sent to a cloud service.

## Tools and tests

GET /v1/tools lists safe registered tools. POST /v1/tools/call executes a named allowlisted function (clock.utc or math.add) with strictly validated arguments. This is the tool boundary only, **not** automatic model-initiated tool calling.

From the repository root:

    python3 tools/generate_face_themes.py --check
    python3 -m unittest discover -s server/tests -v

No packages beyond Python's standard library are required. Command queues are in-memory and removed on polling; there is no delivery ack/retry or persistence yet. mDNS discovery, BLE provisioning, live microphone streaming, TTS and synchronized pan/tilt are subsequent work.
