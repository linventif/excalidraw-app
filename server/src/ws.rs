//! Socket.IO relay implementing the 8 events used by the vendored
//! `excalidraw-app` collab client (see `collab/Portal.tsx` and
//! `collab/Collab.tsx` upstream):
//!
//! Client -> server: `join-room`, `server-broadcast`, `server-volatile-broadcast`, `user-follow`
//! Server -> client: `init-room`, `new-user`, `room-user-change`, `client-broadcast`,
//!                    `first-in-room`, `user-follow-room-change`
//!
//! The server is protocol-blind: every payload it relays is an
//! already-AES-GCM-encrypted blob from the client. It only implements
//! room join/broadcast/presence semantics, mirroring the official
//! `excalidraw-room` Node relay.
//!
//! # The `room-user-change` array-serialization bug (and the fix)
//!
//! socketioxide's `emit(event, data)` serializes `data` to a `serde_json::Value`
//! and, if that value is itself a top-level **non-empty JSON array**, treats
//! each element as a *separate* wire argument (this is what lets you write
//! `socket.emit("test", ("a", "b", 1))` to emit 3 args). A bare `Vec<String>`
//! serializes to a top-level array, so `socket.emit("room-user-change", &vec)`
//! unpacks the socket ids as N separate arguments instead of sending the
//! array itself as a single argument -- which is what the JS client's
//! `(clients: SocketId[]) => ...` handler expects. The fix is to wrap the
//! vec in a 1-tuple, `(vec,)`, so it serializes to `[[...]]`: a single-element
//! top-level array whose one element is the socket id array, which emits as
//! exactly one argument (the array). Verified against a real
//! `socket.io-client` in `tests/protocol_test.js`.
use serde::Deserialize;
use socketioxide::{
    extract::{Bin, Data, SocketRef, State, TryData},
    handler::ConnectHandler,
    socket::Sid,
    SocketIo,
};

use crate::state::AppState;

/// Sent by the client during `io(url, { auth: { token } })`.
#[derive(Debug, Deserialize)]
struct AuthPayload {
    token: String,
}

#[derive(Debug)]
struct AuthError;

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid or missing instance token")
    }
}
impl std::error::Error for AuthError {}

/// Matches `UserToFollow` / `OnUserFollowedPayload` in
/// `packages/excalidraw/types.ts` upstream.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserToFollowPayload {
    socket_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserFollowPayload {
    user_to_follow: UserToFollowPayload,
    action: String,
}

/// A JSON value that serializes to an empty array, which socketioxide emits
/// as zero wire arguments (see module docs on array-splicing behavior).
fn no_args() -> serde_json::Value {
    serde_json::Value::Array(Vec::new())
}

/// Registers the `/` namespace: connect middleware (instance-token auth) +
/// the connection handler wiring up all per-socket event listeners.
pub fn register(io: &SocketIo) {
    io.ns("/", on_connect.with(check_instance_token));
}

/// Connect middleware: validates `Authorization` via the `auth` handshake
/// payload against `INSTANCE_TOKEN`. Uses `TryData` (never fails to extract)
/// rather than `Data` so we can log *why* a handshake was rejected --
/// missing/malformed payload vs. wrong token -- instead of letting a
/// deserialize error silently reject the connection.
fn check_instance_token(
    socket: SocketRef,
    TryData(payload): TryData<AuthPayload>,
    State(state): State<AppState>,
) -> Result<(), AuthError> {
    match payload {
        Ok(p) if p.token == state.config.instance_token => Ok(()),
        Ok(_) => {
            tracing::warn!(socket_id = %socket.id, "socket.io handshake rejected: wrong instance token");
            Err(AuthError)
        }
        Err(err) => {
            tracing::warn!(socket_id = %socket.id, error = %err, "socket.io handshake rejected: missing/malformed auth payload");
            Err(AuthError)
        }
    }
}

fn on_connect(socket: SocketRef) {
    tracing::info!(socket_id = %socket.id, "socket connected");

    // Mirrors excalidraw-room: sent immediately on connect. The client
    // responds by emitting `join-room`.
    let _ = socket.emit("init-room", no_args());

    socket.on("join-room", on_join_room);
    socket.on("server-broadcast", on_server_broadcast);
    socket.on("server-volatile-broadcast", on_server_broadcast);
    socket.on("user-follow", on_user_follow);
    socket.on_disconnect(on_disconnect);
}

fn on_join_room(socket: SocketRef, Data(room_id): Data<String>) {
    if let Err(err) = socket.join(room_id.clone()) {
        tracing::warn!(socket_id = %socket.id, room_id, %err, "failed to join room");
        return;
    }

    let members = socket.within(room_id.clone()).sockets().unwrap_or_default();

    if members.len() <= 1 {
        // Lone client: tell it to load the persisted scene itself instead of
        // waiting for a peer to send one.
        let _ = socket.emit("first-in-room", no_args());
    } else {
        let _ = socket
            .to(room_id.clone())
            .emit("new-user", socket.id.to_string());
    }

    let ids: Vec<String> = members.iter().map(|s| s.id.to_string()).collect();
    // Wrapped in a 1-tuple: see the array-serialization note in the module
    // docs. Without this, socketioxide would emit each id as a separate
    // argument instead of a single `SocketId[]` array argument.
    let _ = socket
        .within(room_id.clone())
        .emit("room-user-change", (ids,));

    tracing::info!(
        socket_id = %socket.id,
        room_id,
        member_count = members.len(),
        "joined room"
    );
}

/// Shared handler for both `server-broadcast` and `server-volatile-broadcast`:
/// relays the encrypted payload to everyone else in the room as
/// `client-broadcast`. The wire shape is `(roomId, encryptedBuffer, iv)` from
/// the client -- `encryptedBuffer`/`iv` travel as raw binary attachments, not
/// JSON, so after socketioxide strips their placeholders out of `data` only
/// `roomId` is left (and gets auto-unwrapped to a bare string). We relay the
/// binary payloads as-is (order preserved) and emit zero extra JSON args, so
/// the client's `(encryptedData, iv) => ...` handler lines up positionally.
fn on_server_broadcast(socket: SocketRef, Data(room_id): Data<String>, Bin(bin): Bin) {
    let _ = socket.to(room_id).bin(bin).emit("client-broadcast", no_args());
}

fn on_user_follow(socket: SocketRef, io: SocketIo, Data(payload): Data<UserFollowPayload>) {
    let follow_room = format!("follow@{}", payload.user_to_follow.socket_id);

    match payload.action.as_str() {
        "FOLLOW" => {
            let _ = socket.join(follow_room.clone());
        }
        "UNFOLLOW" => {
            let _ = socket.leave(follow_room.clone());
        }
        other => {
            tracing::warn!(socket_id = %socket.id, action = other, "unknown user-follow action, ignoring");
            return;
        }
    }

    let follower_ids: Vec<String> = socket
        .within(follow_room)
        .sockets()
        .unwrap_or_default()
        .iter()
        .map(|s| s.id.to_string())
        .collect();

    if let Some(target) = find_socket(&io, &payload.user_to_follow.socket_id) {
        let _ = target.emit("user-follow-room-change", (follower_ids,));
    } else {
        tracing::debug!(
            target_socket_id = %payload.user_to_follow.socket_id,
            "user-follow target is not (or no longer) connected"
        );
    }
}

/// On disconnect: for every room the socket was in (collab rooms and
/// `follow@*` rooms alike -- socketioxide only removes the socket from the
/// adapter's room maps *after* this handler returns, so `socket.rooms()`
/// still reflects full membership here), notify whoever needs to know:
/// - collab room: remaining members get an updated `room-user-change`.
/// - follow room: if it's now empty, the followed user gets notified their
///   follower list dropped to zero.
fn on_disconnect(socket: SocketRef, io: SocketIo) {
    tracing::info!(socket_id = %socket.id, "socket disconnected");

    let rooms = match socket.rooms() {
        Ok(r) => r,
        Err(_) => return,
    };

    for room in rooms {
        // `.to()` (unlike `.within()`) excludes the disconnecting socket
        // itself, so `remaining` is already "everyone else in the room".
        let remaining: Vec<String> = socket
            .to(room.clone())
            .sockets()
            .unwrap_or_default()
            .iter()
            .map(|s| s.id.to_string())
            .collect();

        if let Some(target_id) = room.strip_prefix("follow@") {
            if remaining.is_empty() {
                if let Some(target) = find_socket(&io, target_id) {
                    let _ = target.emit("user-follow-room-change", (Vec::<String>::new(),));
                }
            }
        } else if !remaining.is_empty() {
            let _ = socket
                .to(room.clone())
                .emit("room-user-change", (remaining,));
        }
    }
}

fn find_socket(io: &SocketIo, id: &str) -> Option<SocketRef> {
    if let Ok(sid) = id.parse::<Sid>() {
        if let Some(s) = io.get_socket(sid) {
            return Some(s);
        }
    }
    io.sockets()
        .ok()?
        .into_iter()
        .find(|s| s.id.to_string() == id)
}
