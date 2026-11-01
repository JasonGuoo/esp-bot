// Fake ESP Bot — drives the REAL bot-server the same way firmware lan_link
// will: mDNS TXT gives host/port, WebSocket hello/welcome handshake, then
// optional event frames / waiting for server command frames and acking them.
//
// Zero dependencies (Node >= 22 native WebSocket).
//
//   node scripts/fake-bot.mjs --port 62803 --device-id real-bot-01 --name "Real Test Bot" \
//        [--expect-server-id <id>] [--send-event] [--wait-command] [--hold-ms 1500]
//
// Exit code 0 = scenario completed, 1 = mismatch/timeout.

function arg(name, fallback = undefined) {
  const i = process.argv.indexOf(`--${name}`);
  return i === -1 ? fallback : process.argv[i + 1];
}

const port = Number(arg("port", 0));
const deviceId = arg("device-id", "fake-bot-01");
const name = arg("name", "Fake Bot");
const expectServerId = arg("expect-server-id");
const sendEvent = process.argv.includes("--send-event");
const waitCommand = process.argv.includes("--wait-command");
const holdMs = Number(arg("hold-ms", "1500"));
const commandCount = Number(arg("count", "1"));

if (!port) {
  console.error("usage: node fake-bot.mjs --port <port> [--device-id id] [--name n] [--expect-server-id id] [--send-event] [--wait-command]");
  process.exit(1);
}

const url = `ws://127.0.0.1:${port}/v1/lan/ws`;
const socket = new WebSocket(url);
const defaultTimeout = waitCommand ? 120000 : 15000;
const deadline = Date.now() + Number(arg("timeout-ms", String(defaultTimeout)));
let finished = false;
let receivedCommands = 0;
const fail = (message) => {
  if (finished) return;
  finished = true;
  console.error(`FAIL: ${message}`);
  try { socket.close(); } catch {}
  process.exit(1);
};

console.log(`connecting ${url} as ${deviceId} (${name})`);
socket.onopen = () => {
  console.log("open, sending hello");
  socket.send(JSON.stringify({ type: "hello", protocol: 1, device_id: deviceId, name }));
  if (sendEvent) {
    setTimeout(() => {
      console.log("sending event frame button.pressed");
      socket.send(JSON.stringify({ type: "event", topic: "button.pressed", value: 1 }));
    }, 300);
  }
};

socket.onmessage = (message) => {
  if (finished) return;
  let frame;
  try {
    frame = JSON.parse(message.data.toString());
  } catch {
    return fail(`non-JSON frame: ${message.data}`);
  }
  if (frame.type === "welcome") {
    console.log(`welcome: protocol=${frame.protocol} server_id=${frame.server_id}`);
    if (expectServerId && frame.server_id !== expectServerId) {
      return fail(`server_id mismatch: got ${frame.server_id}, expected ${expectServerId}`);
    }
    if (!waitCommand) {
      setTimeout(() => {
        console.log("scenario complete, closing");
        socket.close(1000, "done");
        process.exit(0);
      }, holdMs);
    } else {
      console.log("waiting for a command frame…");
    }
  } else if (frame.type === "request") {
    console.log(`command received: id=${frame.id} method=${frame.method} payload=${JSON.stringify(frame.payload)}`);
    const ack = { v: 2, type: "response", id: frame.id, ok: true, detail: "applied by fake bot" };
    socket.send(JSON.stringify(ack));
    console.log(`acked ${frame.id} (${receivedCommands + 1}/${commandCount})`);
    receivedCommands += 1;
    if (receivedCommands >= commandCount) {
      console.log("scenario complete");
      setTimeout(() => process.exit(0), 300);
    }
  } else {
    console.log(`frame: ${JSON.stringify(frame)}`);
  }
};

socket.onerror = (event) => fail(`websocket error: ${event.message ?? "unknown"}`);
socket.onclose = (event) => {
  if (finished) return;
  finished = true;
  console.log(`closed: code=${event.code} reason=${event.reason || "(none)"}`);
  process.exit(event.code === 4003 ? 3 : 0);
};

setTimeout(() => fail("overall timeout"), deadline - Date.now());
