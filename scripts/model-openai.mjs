// Model adapter for `npm run agent`: reads a prompt on stdin, sends it to an OpenAI-compatible
// chat endpoint, prints the reply. Works with a local llama.cpp server, Ollama or LM Studio.
//   PREAVIS_MODEL_URL      default http://127.0.0.1:8080/v1/chat/completions (llama.cpp server)
//   PREAVIS_MODEL_NAME     the "model" field, default "local"
//   PREAVIS_MODEL_API_KEY  optional bearer token. A hosted endpoint with your key bills your account.
let prompt = "";
for await (const chunk of process.stdin) prompt += chunk;
const url = process.env.PREAVIS_MODEL_URL ?? "http://127.0.0.1:8080/v1/chat/completions";
const headers = { "content-type": "application/json" };
if (process.env.PREAVIS_MODEL_API_KEY) headers.authorization = `Bearer ${process.env.PREAVIS_MODEL_API_KEY}`;
const response = await fetch(url, {
  method: "POST",
  headers,
  body: JSON.stringify({
    model: process.env.PREAVIS_MODEL_NAME ?? "local",
    messages: [{ role: "user", content: prompt }],
    temperature: 0,
    seed: 0,
    max_tokens: 300,
  }),
});
if (!response.ok) throw new Error(`${url}: HTTP ${response.status} ${(await response.text()).slice(0, 300)}`);
process.stdout.write((await response.json()).choices[0].message.content);
