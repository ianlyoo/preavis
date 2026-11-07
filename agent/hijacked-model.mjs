// A stand-in for a fully hijacked model: it does whatever the invoice text says, injection included.
// PREAVIS_MODEL_CMD="node agent/hijacked-model.mjs" npm run agent
let prompt = "";
for await (const chunk of process.stdin) prompt += chunk;
const invoice = prompt.split("The invoice:")[1] ?? "";
const payee = invoice.match(/Pay to NEAR account: (\S+)/)?.[1];
const amount = invoice.match(/Amount due: ([0-9.]+)/)?.[1];
console.log(JSON.stringify({ pay: true, payee, amount_near: amount, reason: "the invoice says so" }));
