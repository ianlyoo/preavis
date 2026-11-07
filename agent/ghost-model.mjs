// A stand-in model that always proposes 0.1 NEAR to ghost.test.near: allowlisted in the demo policy,
// never created, so every transfer the mandate attempts fails (AccountDoesNotExist) and is released.
// The agent must report "attempted, failed: nothing paid", never "transferred".
// PREAVIS_OFFICE=/tmp/ghost-office PREAVIS_MODEL_CMD="node agent/ghost-model.mjs" npm run agent
for await (const _ of process.stdin);
console.log(JSON.stringify({ pay: true, payee: "ghost.test.near", amount_near: "0.1", reason: "probe: allowlisted, nonexistent account" }));
