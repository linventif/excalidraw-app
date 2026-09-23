#!/usr/bin/env node
/**
 * Auth protocol interop test for `AUTH_MODE=open-registration`.
 *
 * Spawns the real compiled server binary and drives the FULL real
 * challenge/verify flow using the REAL `tweetnacl` / `tweetnacl-util`
 * libraries (not a mock, and not the Rust `crypto_box` crate acting as a
 * stand-in client, unlike `tests/auth_test.rs`) -- the exact libraries the
 * vendored `excalidraw-app/data/identity.ts` uses in the real desktop app.
 *
 * This is the critical wire-format fidelity check: it proves the Rust
 * server's `crypto_box::SalsaBox` (XSalsa20-Poly1305 + X25519) produces
 * ciphertext that `nacl.box.open()` can actually decrypt, and that every
 * field name/shape (`publicKey`, `challengeId`, `serverPublicKey`, `nonce`,
 * `encryptedChallenge`, `solution`, `username`, `sessionToken`, `expiresAt`)
 * matches the already-shipped client in `excalidraw-app/data/identity.ts`
 * byte-for-byte. This is exactly the kind of check that caught the
 * `room-user-change` array-serialization bug in `tests/protocol_test.js` --
 * a Rust-only test can silently agree with itself about a wire format that
 * doesn't actually match a real JS client.
 *
 * Flow covered:
 *   1. nacl.box.keyPair() -> POST /auth/challenge
 *   2. nacl.box.open() the response with the real client secret key
 *   3. POST /auth/verify with the recovered plaintext
 *   4. use the returned sessionToken as a Bearer token against a protected
 *      REST route and confirm it's accepted (404, not 401)
 *   5. re-run the same identity through the flow and confirm the same
 *      username comes back (device re-authentication is stable)
 *
 * Exits 0 if every assertion passed, 1 otherwise.
 */

"use strict";

const path = require("path");
const fs = require("fs");
const os = require("os");
const http = require("http");
const { spawn } = require("child_process");

// tweetnacl/tweetnacl-util are yarn-workspace deps of excalidraw-app, but get
// hoisted to the *workspace root* node_modules (unlike socket.io-client,
// which yarn keeps local to excalidraw-app/node_modules -- presumably due to
// a version pin elsewhere in the monorepo). Try the local path first in case
// a future install layout puts them there, then fall back to the hoisted
// root location actually observed in this repo.
function resolveNaclModule(name) {
  const candidates = [
    path.resolve(__dirname, "../../vendor/excalidraw/excalidraw-app/node_modules", name),
    path.resolve(__dirname, "../../vendor/excalidraw/node_modules", name),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return require(candidate);
    }
  }
  throw new Error(
    `could not find "${name}" in any of:\n  ${candidates.join("\n  ")}\n` +
      `(run \`yarn install\` in vendor/excalidraw first)`,
  );
}

const nacl = resolveNaclModule("tweetnacl");
const naclUtil = resolveNaclModule("tweetnacl-util");

const SERVER_ROOT = path.resolve(__dirname, "..");
const PORT = 13581;
const BASE_URL = `http://127.0.0.1:${PORT}`;

// ---------------------------------------------------------------------------
// tiny test harness (mirrors protocol_test.js)
// ---------------------------------------------------------------------------

let passed = 0;
let failed = 0;
const failures = [];

function ok(cond, msg) {
  if (cond) {
    passed++;
    console.log(`  \x1b[32mPASS\x1b[0m ${msg}`);
  } else {
    failed++;
    failures.push(msg);
    console.log(`  \x1b[31mFAIL\x1b[0m ${msg}`);
  }
}

function postJson(pathName, body) {
  return new Promise((resolve, reject) => {
    const data = Buffer.from(JSON.stringify(body), "utf8");
    const req = http.request(
      `${BASE_URL}${pathName}`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Content-Length": data.length,
        },
      },
      (res) => {
        let raw = "";
        res.on("data", (chunk) => (raw += chunk));
        res.on("end", () => {
          let json = null;
          try {
            json = JSON.parse(raw);
          } catch (_e) {
            // leave json null; caller checks status separately
          }
          resolve({ status: res.statusCode, json, raw });
        });
      },
    );
    req.on("error", reject);
    req.write(data);
    req.end();
  });
}

function getWithAuth(pathName, token) {
  return new Promise((resolve, reject) => {
    const headers = token ? { Authorization: `Bearer ${token}` } : {};
    const req = http.request(`${BASE_URL}${pathName}`, { method: "GET", headers }, (res) => {
      res.resume();
      res.on("end", () => resolve({ status: res.statusCode }));
    });
    req.on("error", reject);
    req.end();
  });
}

function waitForHealth(timeoutMs = 10000) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const req = http.get(`${BASE_URL}/health`, (res) => {
        res.resume();
        if (res.statusCode === 200) {
          resolve();
        } else if (Date.now() > deadline) {
          reject(new Error(`/health returned ${res.statusCode}`));
        } else {
          setTimeout(attempt, 150);
        }
      });
      req.on("error", () => {
        if (Date.now() > deadline) {
          reject(new Error("server never became healthy"));
        } else {
          setTimeout(attempt, 150);
        }
      });
    };
    attempt();
  });
}

function resolveServerBinary() {
  const release = path.join(SERVER_ROOT, "target/release/excalidraw-server");
  const debug = path.join(SERVER_ROOT, "target/debug/excalidraw-server");
  if (fs.existsSync(release)) return release;
  if (fs.existsSync(debug)) return debug;
  throw new Error(
    "no server binary found; run `cargo build` or `cargo build --release` in server/ first",
  );
}

// ---------------------------------------------------------------------------
// the real NaCl client-side flow (mirrors excalidraw-app/data/identity.ts)
// ---------------------------------------------------------------------------

function generateIdentity() {
  const pair = nacl.box.keyPair();
  return {
    publicKey: naclUtil.encodeBase64(pair.publicKey),
    secretKeyBytes: pair.secretKey,
  };
}

/** Full registerOrLogin() dance, real tweetnacl crypto throughout. */
async function registerOrLogin(identity) {
  const challengeRes = await postJson("/auth/challenge", { publicKey: identity.publicKey });
  ok(challengeRes.status === 200, `POST /auth/challenge returns 200 (got ${challengeRes.status})`);
  const challenge = challengeRes.json;
  ok(
    typeof challenge?.challengeId === "string" &&
      typeof challenge?.serverPublicKey === "string" &&
      typeof challenge?.nonce === "string" &&
      typeof challenge?.encryptedChallenge === "string",
    "challenge response has the exact expected shape (challengeId, serverPublicKey, nonce, encryptedChallenge)",
  );

  const serverPublicKey = naclUtil.decodeBase64(challenge.serverPublicKey);
  const nonce = naclUtil.decodeBase64(challenge.nonce);
  const encrypted = naclUtil.decodeBase64(challenge.encryptedChallenge);

  ok(serverPublicKey.length === 32, "serverPublicKey decodes to exactly 32 bytes");
  ok(nonce.length === 24, "nonce decodes to exactly 24 bytes");

  // THE critical interop assertion: nacl.box.open() -- the real tweetnacl
  // XSalsa20-Poly1305 AEAD -- must be able to decrypt what the Rust server's
  // crypto_box::SalsaBox produced. If the server had used ChaChaBox, a
  // different nonce convention, or mismatched tag placement, this call
  // returns `false` instead of throwing, so we assert on truthiness.
  const decrypted = nacl.box.open(encrypted, nonce, serverPublicKey, identity.secretKeyBytes);
  ok(
    decrypted !== false && decrypted !== null,
    "nacl.box.open() successfully decrypts the server's SalsaBox-encrypted challenge (wire-format interop)",
  );
  ok(decrypted && decrypted.length === 32, "decrypted challenge plaintext is exactly 32 bytes");

  const verifyRes = await postJson("/auth/verify", {
    challengeId: challenge.challengeId,
    solution: naclUtil.encodeBase64(decrypted),
  });
  ok(verifyRes.status === 200, `POST /auth/verify returns 200 (got ${verifyRes.status}, ${verifyRes.raw})`);
  const verified = verifyRes.json;
  ok(
    typeof verified?.username === "string" &&
      typeof verified?.sessionToken === "string" &&
      typeof verified?.expiresAt === "string",
    "verify response has the exact expected shape (username, sessionToken, expiresAt)",
  );
  ok(
    !Number.isNaN(new Date(verified?.expiresAt).getTime()),
    "expiresAt parses as a valid date (ISO 8601)",
  );

  return verified;
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main() {
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "excalidraw-server-auth-test-"));
  const binary = resolveServerBinary();

  console.log(`Using server binary: ${binary}`);
  console.log(`Using data dir: ${dataDir}`);

  const child = spawn(binary, [], {
    env: {
      ...process.env,
      PORT: String(PORT),
      AUTH_MODE: "open-registration",
      DATA_DIR: dataDir,
      DATABASE_PATH: path.join(dataDir, "excalidraw.sqlite"),
      RUST_LOG: "info",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  let serverOutput = "";
  child.stdout.on("data", (d) => (serverOutput += d.toString()));
  child.stderr.on("data", (d) => (serverOutput += d.toString()));

  child.on("exit", (code, signal) => {
    if (code !== null && code !== 0) {
      console.error(`server process exited early with code ${code} (signal ${signal})`);
      console.error(serverOutput);
    }
  });

  try {
    await waitForHealth();
    console.log("Server is healthy (AUTH_MODE=open-registration).\n");

    console.log("Test: full real-tweetnacl challenge/verify round trip");
    const identity = generateIdentity();
    const session = await registerOrLogin(identity);

    console.log("\nTest: the returned sessionToken is accepted on a protected REST route");
    const authed = await getWithAuth("/scenes/does-not-exist-but-auth-should-pass", session.sessionToken);
    ok(
      authed.status === 404,
      `GET /scenes/:room_id with the real session token returns 404, not 401 (got ${authed.status})`,
    );

    console.log("\nTest: an invalid session token is rejected on the same route");
    const unauthed = await getWithAuth("/scenes/does-not-exist-but-auth-should-pass", "not-a-real-token");
    ok(unauthed.status === 401, `GET /scenes/:room_id with a bogus token returns 401 (got ${unauthed.status})`);

    console.log("\nTest: missing auth is rejected too");
    const noAuth = await getWithAuth("/scenes/does-not-exist-but-auth-should-pass", null);
    ok(noAuth.status === 401, `GET /scenes/:room_id with no Authorization header returns 401 (got ${noAuth.status})`);

    console.log("\nTest: re-authenticating the same real identity returns the same username");
    const session2 = await registerOrLogin(identity);
    ok(
      session2.username === session.username,
      `same device gets the same username back (${session.username} === ${session2.username})`,
    );
    ok(
      session2.sessionToken !== session.sessionToken,
      "but a fresh session token each time",
    );
  } catch (err) {
    console.error("Unexpected error running test suite:", err);
    failed++;
    failures.push(`unexpected exception: ${err.message}`);
  } finally {
    child.kill();
    fs.rmSync(dataDir, { recursive: true, force: true });
  }

  console.log(`\n${passed} passed, ${failed} failed`);
  if (failed > 0) {
    console.log("\nFailures:");
    for (const f of failures) console.log(`  - ${f}`);
    process.exit(1);
  }
  process.exit(0);
}

main();
