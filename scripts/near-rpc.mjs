// A minimal NEAR testnet JSON-RPC client for the read-only scripts: no key, no dependencies (Node >= 18).
// PREAVIS_TESTNET_RPC=<url> replaces the default endpoint list.
import { createHash } from "node:crypto";

const endpoints = process.env.PREAVIS_TESTNET_RPC
  ? [process.env.PREAVIS_TESTNET_RPC]
  : ["https://archival-rpc.testnet.fastnear.com", "https://test.rpc.fastnear.com", "https://archival-rpc.testnet.near.org"];

// Retries rate limits (429) and server errors with backoff, then tries the next endpoint.
export async function rpc(method, params) {
  for (const url of endpoints) {
    for (let attempt = 0; attempt < 4; attempt++) {
      try {
        const response = await fetch(url, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
        });
        if (response.status === 429 || response.status >= 500) throw new Error(`HTTP ${response.status}`);
        const body = await response.json();
        if (body.error) throw new Error(JSON.stringify(body.error).slice(0, 300));
        return body.result;
      } catch (error) {
        if (!/HTTP (429|5)|fetch failed/.test(error.message)) throw error;
        await new Promise((resolve) => setTimeout(resolve, 1000 * 2 ** attempt));
      }
    }
  }
  throw new Error(`every RPC endpoint failed for ${method}; set PREAVIS_TESTNET_RPC to another testnet RPC`);
}

export const query = (params) => rpc("query", { finality: "final", ...params });

export async function view(account, method, args) {
  const result = await query({
    request_type: "call_function", account_id: account, method_name: method,
    args_base64: Buffer.from(JSON.stringify(args)).toString("base64"),
  });
  return JSON.parse(Buffer.from(result.result).toString());
}

export const sha256 = (bytes) => createHash("sha256").update(bytes).digest();

// NEAR shows code hashes as base58 of the sha256.
export function base58(bytes) {
  const alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let n = BigInt("0x" + Buffer.from(bytes).toString("hex"));
  let out = "";
  while (n > 0n) { out = alphabet[Number(n % 58n)] + out; n /= 58n; }
  for (const b of bytes) { if (b !== 0) break; out = "1" + out; }
  return out;
}
