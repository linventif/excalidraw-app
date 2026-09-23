#!/usr/bin/env node
/**
 * Socket.IO protocol integration test for the excalidraw collab server.
 *
 * Spawns the real compiled server binary and exercises it with the REAL
 * `socket.io-client@4.7.2` vendored for the excalidraw-app (not a mock), the
 * same client version the desktop app actually uses. Covers:
 *
 *   - auth rejection: missing token / wrong token (socket.io handshake)
 *   - join-room -> first-in-room for a lone client
 *   - join-room -> new-user + room-user-change when a second client joins
 *   - the room-user-change array-serialization bug: asserts the event is
 *     received as *one* array argument, not N separate string arguments
 *   - server-broadcast -> client-broadcast relay with real binary payloads
 *     (Buffers), asserting the 2-argument (encryptedData, iv) shape survives
 *   - disconnect -> room-user-change update for remaining members
 *   - user-follow / user-follow-room-change, including array-wrapping and
 *     the disconnect-triggered follower cleanup path
 *
 * Exits 0 if every assertion passed, 1 otherwise.
 */

"use strict";

const path = require("path");
const fs = require("fs");
const os = require("os");
const http = require("http");
const { spawn } = require("child_process");

const SOCKET_IO_CLIENT_PATH = path.resolve(
  __dirname,
  "../../vendor/excalidraw/excalidraw-app/node_modules/socket.io-client",
);
const { io } = require(SOCKET_IO_CLIENT_PATH);

const SERVER_ROOT = path.resolve(__dirname, "..");
const PORT = 13580;
const BASE_URL = `http://127.0.0.1:${PORT}`;
const INSTANCE_TOKEN = "test-instance-token-12345";

// ---------------------------------------------------------------------------
// tiny test harness
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

function deepEqualUnordered(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b)) return false;
  if (a.length !== b.length) return false;
  const sa = [...a].sort();
  const sb = [...b].sort();
  return sa.every((v, i) => v === sb[i]);
}

function waitForEvent(socket, event, timeoutMs = 4000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`timed out waiting for "${event}"`));
    }, timeoutMs);
    socket.once(event, (...args) => {
      clearTimeout(timer);
      resolve(args);
    });
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function newClient(opts = {}) {
  const socket = io(BASE_URL, {
    transports: ["websocket", "polling"],
    reconnection: false,
    forceNew: true,
    auth: opts.auth,
  });
  openSockets.push(socket);
  return socket;
}

const openSockets = [];

// ---------------------------------------------------------------------------
// server process management
// ---------------------------------------------------------------------------

function resolveServerBinary() {
  const release = path.join(SERVER_ROOT, "target/release/excalidraw-server");
  const debug = path.join(SERVER_ROOT, "target/debug/excalidraw-server");
  if (fs.existsSync(release)) return release;
  if (fs.existsSync(debug)) return debug;
  throw new Error(
    "no server binary found; run `cargo build` or `cargo build --release` in server/ first",
  );
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

async function main() {
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "excalidraw-server-test-"));
  const binary = resolveServerBinary();

  console.log(`Using server binary: ${binary}`);
  console.log(`Using data dir: ${dataDir}`);

  const child = spawn(binary, [], {
    env: {
      ...process.env,
      PORT: String(PORT),
      INSTANCE_TOKEN,
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
    console.log("Server is healthy.\n");

    await testAuthRejectsMissingToken();
    await testAuthRejectsWrongToken();
    await testJoinRoomAloneFirstInRoom();
    await testSecondClientJoinsRoom();
    await testBroadcastRelay();
    await testVolatileBroadcastRelay();
    await testDisconnectUpdatesRoomUserChange();
    await testUserFollowFlow();
  } catch (err) {
    console.error("Unexpected error running test suite:", err);
    failed++;
    failures.push(`unexpected exception: ${err.message}`);
  } finally {
    for (const s of openSockets) {
      try {
        s.close();
      } catch (_e) {
        // ignore
      }
    }
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

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

async function testAuthRejectsMissingToken() {
  console.log("Test: handshake rejected when auth token is missing");
  const socket = newClient({});
  try {
    await waitForEvent(socket, "connect_error", 3000);
    ok(true, "connect_error fired for missing auth token");
  } catch (err) {
    ok(false, `expected connect_error, got: ${err.message}`);
  }
  ok(!socket.connected, "socket never reached connected state");
  socket.close();
}

async function testAuthRejectsWrongToken() {
  console.log("Test: handshake rejected when auth token is wrong");
  const socket = newClient({ auth: { token: "definitely-not-the-token" } });
  try {
    await waitForEvent(socket, "connect_error", 3000);
    ok(true, "connect_error fired for wrong auth token");
  } catch (err) {
    ok(false, `expected connect_error, got: ${err.message}`);
  }
  ok(!socket.connected, "socket never reached connected state");
  socket.close();
}

async function testJoinRoomAloneFirstInRoom() {
  console.log("Test: lone client joining a room gets first-in-room");
  const socket = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(socket, "connect", 3000);
  ok(socket.connected, "socket connected with correct token");

  const roomId = "room-solo-" + Math.random().toString(36).slice(2);
  const firstInRoomPromise = waitForEvent(socket, "first-in-room");
  const roomChangePromise = waitForEvent(socket, "room-user-change");
  socket.emit("join-room", roomId);

  // The real client's handler (`socket.on("first-in-room", async () => {...})`)
  // takes no parameters, so any argument socketioxide's `emit()` tacks on
  // (it can't express a true zero-argument event; an empty-array payload
  // comes through as one `[]` argument) is harmless and ignored by the
  // client. We only assert the event actually fires.
  await firstInRoomPromise;
  ok(true, "first-in-room event received");

  const roomChangeArgs = await roomChangePromise;
  ok(
    roomChangeArgs.length === 1 && Array.isArray(roomChangeArgs[0]),
    "room-user-change delivered as exactly one array argument (not spread as varargs)",
  );
  ok(
    Array.isArray(roomChangeArgs[0]) &&
      roomChangeArgs[0].length === 1 &&
      roomChangeArgs[0][0] === socket.id,
    "room-user-change array contains exactly this socket's id",
  );

  socket.close();
}

async function testSecondClientJoinsRoom() {
  console.log("Test: second client joining triggers new-user + room-user-change for both");
  const roomId = "room-pair-" + Math.random().toString(36).slice(2);

  const a = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(a, "connect", 3000);
  const aFirstInRoom = waitForEvent(a, "first-in-room");
  a.emit("join-room", roomId);
  await aFirstInRoom;

  const b = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(b, "connect", 3000);

  const newUserPromise = waitForEvent(a, "new-user");
  const aRoomChangePromise = waitForEvent(a, "room-user-change");
  const bRoomChangePromise = waitForEvent(b, "room-user-change");
  b.emit("join-room", roomId);

  const newUserArgs = await newUserPromise;
  ok(
    newUserArgs.length === 1 && newUserArgs[0] === b.id,
    "existing member (A) receives new-user with the joining socket's id",
  );

  const aRoomChangeArgs = await aRoomChangePromise;
  ok(
    aRoomChangeArgs.length === 1 && Array.isArray(aRoomChangeArgs[0]),
    "A's room-user-change delivered as a single array argument",
  );
  ok(
    deepEqualUnordered(aRoomChangeArgs[0], [a.id, b.id]),
    "A's room-user-change contains both member ids",
  );

  const bRoomChangeArgs = await bRoomChangePromise;
  ok(
    bRoomChangeArgs.length === 1 &&
      deepEqualUnordered(bRoomChangeArgs[0], [a.id, b.id]),
    "B's room-user-change also contains both member ids (within() includes self)",
  );

  a.close();
  b.close();
}

async function testBroadcastRelay() {
  console.log("Test: server-broadcast relays real binary payloads as client-broadcast(data, iv)");
  await broadcastRelayCase("server-broadcast");
}

async function testVolatileBroadcastRelay() {
  console.log("Test: server-volatile-broadcast relays the same way");
  await broadcastRelayCase("server-volatile-broadcast");
}

async function broadcastRelayCase(eventName) {
  const roomId = "room-broadcast-" + Math.random().toString(36).slice(2);

  const a = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(a, "connect", 3000);
  const aFirstInRoom = waitForEvent(a, "first-in-room");
  a.emit("join-room", roomId);
  await aFirstInRoom;

  const b = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(b, "connect", 3000);
  const bJoined = waitForEvent(b, "room-user-change");
  const aSeesJoin = waitForEvent(a, "room-user-change");
  b.emit("join-room", roomId);
  await Promise.all([bJoined, aSeesJoin]);

  const encryptedBuffer = Buffer.from("not-real-ciphertext-but-looks-like-it-ünïcödé", "utf8");
  const iv = Buffer.from([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

  const relayPromise = waitForEvent(a, "client-broadcast");
  b.emit(eventName, roomId, encryptedBuffer, iv);
  const relayArgs = await relayPromise;

  ok(
    relayArgs.length === 2,
    `${eventName}: client-broadcast delivered with exactly 2 arguments (encryptedData, iv)`,
  );
  const [receivedData, receivedIv] = relayArgs;
  ok(
    Buffer.isBuffer(receivedData) && Buffer.compare(receivedData, encryptedBuffer) === 0,
    `${eventName}: encrypted payload bytes survive the relay unmodified`,
  );
  ok(
    Buffer.isBuffer(receivedIv) && Buffer.compare(receivedIv, iv) === 0,
    `${eventName}: iv bytes survive the relay unmodified`,
  );

  // The sender must NOT receive its own broadcast back (mirrors `.to()`
  // excluding the current socket, like the original `socket.broadcast.to`).
  let sawEcho = false;
  b.once("client-broadcast", () => {
    sawEcho = true;
  });
  await sleep(300);
  ok(!sawEcho, `${eventName}: sender does not receive its own broadcast echoed back`);

  a.close();
  b.close();
}

async function testDisconnectUpdatesRoomUserChange() {
  console.log("Test: disconnecting updates room-user-change for remaining members");
  const roomId = "room-leave-" + Math.random().toString(36).slice(2);

  const a = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(a, "connect", 3000);
  const aFirstInRoom = waitForEvent(a, "first-in-room");
  a.emit("join-room", roomId);
  await aFirstInRoom;

  const b = newClient({ auth: { token: INSTANCE_TOKEN } });
  await waitForEvent(b, "connect", 3000);
  const aSeesBJoin = waitForEvent(a, "room-user-change");
  b.emit("join-room", roomId);
  await aSeesBJoin;

  const aSeesBLeave = waitForEvent(a, "room-user-change");
  b.close();
  const leaveArgs = await aSeesBLeave;

  ok(
    leaveArgs.length === 1 && Array.isArray(leaveArgs[0]),
    "room-user-change after disconnect is still a single array argument",
  );
  ok(
    deepEqualUnordered(leaveArgs[0], [a.id]),
    "room-user-change after B disconnects contains only A's id",
  );

  a.close();
}

async function testUserFollowFlow() {
  console.log("Test: user-follow / user-follow-room-change (including disconnect cleanup)");

  const c = newClient({ auth: { token: INSTANCE_TOKEN } });
  const d = newClient({ auth: { token: INSTANCE_TOKEN } });
  await Promise.all([waitForEvent(c, "connect", 3000), waitForEvent(d, "connect", 3000)]);

  // FOLLOW
  const followChange1 = waitForEvent(d, "user-follow-room-change");
  c.emit("user-follow", {
    userToFollow: { socketId: d.id, username: "d" },
    action: "FOLLOW",
  });
  const followArgs1 = await followChange1;
  ok(
    followArgs1.length === 1 && Array.isArray(followArgs1[0]),
    "user-follow-room-change delivered as a single array argument",
  );
  ok(
    deepEqualUnordered(followArgs1[0], [c.id]),
    "followed user (D) sees C in its follower list after FOLLOW",
  );

  // UNFOLLOW
  const followChange2 = waitForEvent(d, "user-follow-room-change");
  c.emit("user-follow", {
    userToFollow: { socketId: d.id, username: "d" },
    action: "UNFOLLOW",
  });
  const followArgs2 = await followChange2;
  ok(
    Array.isArray(followArgs2[0]) && followArgs2[0].length === 0,
    "follower list is empty after UNFOLLOW",
  );

  // FOLLOW again, then disconnect the follower -> cleanup path
  const followChange3 = waitForEvent(d, "user-follow-room-change");
  c.emit("user-follow", {
    userToFollow: { socketId: d.id, username: "d" },
    action: "FOLLOW",
  });
  await followChange3;

  const followChange4 = waitForEvent(d, "user-follow-room-change");
  c.close();
  const followArgs4 = await followChange4;
  ok(
    Array.isArray(followArgs4[0]) && followArgs4[0].length === 0,
    "follower list drops to empty when the follower disconnects (cleanup on disconnect)",
  );

  d.close();
}

main();
