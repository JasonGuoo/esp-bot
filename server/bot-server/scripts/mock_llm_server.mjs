// Minimal OpenAI-compatible HTTP server for real-adapter testing without a
// GPU model: implements GET /v1/models and POST /v1/chat/completions exactly
// as the OpenAiProvider adapter calls them (non-streaming).
//
//   node scripts/mock_llm_server.mjs [port=8765]
//
// The reply echoes the last user message so tests can assert the adapter
// transported the real prompt.

import http from "node:http";

const port = Number(process.argv[2] ?? 8765);

const server = http.createServer((request, response) => {
  const chunks = [];
  request.on("data", (chunk) => chunks.push(chunk));
  request.on("end", () => {
    const body = Buffer.concat(chunks).toString("utf-8");
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "mock-model" }] }));
      return;
    }
    if (request.method === "POST" && request.url === "/v1/chat/completions") {
      let prompt = "?";
      try {
        const payload = JSON.parse(body);
        const messages = payload.messages ?? [];
        const user = messages.filter((m) => m.role === "user").pop();
        prompt = user?.content ?? "?";
        console.log(`[${new Date().toISOString()}] model=${payload.model} stream=${payload.stream} user=${JSON.stringify(prompt)}`);
      } catch (error) {
        response.writeHead(400, { "content-type": "application/json" });
        response.end(JSON.stringify({ error: { message: `bad json: ${error}` } }));
        return;
      }
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          id: "chatcmpl-mock",
          object: "chat.completion",
          choices: [
            {
              index: 0,
              message: { role: "assistant", content: `Mock reply to: ${prompt}` },
              finish_reason: "stop",
            },
          ],
        }),
      );
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: { message: "unknown route" } }));
  });
});

server.listen(port, "127.0.0.1", () => console.log(`mock LLM listening on http://127.0.0.1:${port}/v1`));
