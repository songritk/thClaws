# LINE Bridge (plan-07)

LINE OA ↔ thClaws desktop relay: a user chats with their thClaws session over LINE, the agent runs on their local machine, and a small server in the middle routes messages between LINE Messaging API webhooks and a per-install WebSocket.

| Layer | Lives at | Role |
|---|---|---|
| Client-side bridge | `crates/core/src/line/` | WS client + reply-sender + `LineApprover` + pairing-token config |
| Frontend modal | `frontend/src/components/LineConnectModal.tsx` | Paste pairing code → POST `/pair` → store JWT → start WS |
| Sidebar pill | `frontend/src/components/Sidebar.tsx` | "Bridge live · `<display_name>`" status with avatar |
| Worker integration | `crates/core/src/shared_session.rs` `ShellInput::LineMessage` arm | Drives `Agent::run_turn` per inbound LINE message |
| Official relay | `crates/line-server/` (workspace-only — not in public mirror) | Axum + Redis + Postgres on k3s at `line.thclaws.ai` |

## Why this doc

The LINE bridge is unusual among thClaws surfaces because anyone can write their own relay — the protocol between thClaws and the relay is intentionally narrow and documented. This page is the contract third-party relay implementers code against. The official relay lives outside the public repo (server-side infrastructure), but its wire shape is open.

## Wire protocol

### Client → relay: `POST /pair`

Body:
```json
{ "code": "ABCD1234", "cwd": "/path/to/project", "machine_label": "jimmy-mac" }
```

Successful response:
```json
{
  "token": "<HS256 JWT>",
  "line_user_id": "Uxxx…",
  "expires_at": 1735689600,
  "display_name": "Jimmy",
  "picture_url": "https://profile.line-scdn.net/…",
  "language": "th"
}
```

`display_name` / `picture_url` / `language` are optional — relays without a profile cache omit them (older relays, or `GET /v2/bot/profile/:userId` failure). thClaws falls back to "bridge live" on the sidebar pill when absent.

### Client → relay: `POST /unpair`

Authenticated by `Authorization: Bearer <jwt>`. Drops the binding row + reverse index. Idempotent — already-deleted bindings return 200 with `{"status": "already_clean"}`. Best-effort from the client side: the worker fires this in a detached task on `LineDisconnect` and proceeds with local cleanup regardless of the result.

### Client ↔ relay: WebSocket `/ws?token=<jwt>`

Relay → client envelopes:
```json
{ "kind": "user_message", "text": "…", "reply_token": "…", "request_id": "…" }
{ "kind": "postback", "data": "tool:allow:<request_id>" }
{ "kind": "notice", "text": "…" }
```

The client must support reconnect with exponential backoff — pod restarts during k8s rolling updates drop WS connections, and the official relay's [presence TTL](../thclaws/crates/line-server/src/store.rs) (60 s) is sized to absorb the gap without surfacing a spurious "thClaws offline" pairing code to the user.

### Client → relay: `POST /reply/:request_id`

Authenticated by `Authorization: Bearer <jwt>`. Body:
```json
{ "text": "agent response", "quick_reply": [
  { "label": "Approve", "data": "tool:allow:abc", "display_text": "Approve" },
  { "label": "Deny",    "data": "tool:deny:abc",  "display_text": "Deny" }
] }
```

`quick_reply` is optional. When present, the relay attaches LINE-native postback chips so the user can tap instead of typing approve/deny.

## Implementer guidance: prefer reply API over push

The LINE Messaging API has two outbound paths for `POST /reply/:request_id` to map to:

- **`POST /v2/bot/message/reply`** — uses the cached `replyToken` from the webhook. Free, unlimited within the channel's per-event quota.
- **`POST /v2/bot/message/push`** — direct push to a user. **Counts against the channel's monthly quota** (200/month on free tier; rapid kill if defaulted).

**Always try reply first.** Reply tokens expire 60 seconds after the webhook event and are single-use. Recommended logic:

> Call `POST /v2/bot/message/reply` if the cached `replyToken` is less than ~55 seconds old. Fall back to `POST /v2/bot/message/push` only when the reply token is expired or when the reply API returns an error.

The official relay implements this (`crates/line-server/src/routes/reply.rs`): reply-first, push fallback on any reply-API error. Third-party relays defaulting to push will exhaust the free quota in days under realistic load — verified empirically.

## Profile cache

The official relay maintains a `line_users` Postgres table:

```sql
CREATE TABLE line_users (
    line_user_id        TEXT PRIMARY KEY,
    display_name        TEXT NOT NULL,
    picture_url         TEXT,
    status_message      TEXT,
    language            TEXT,
    profile_fetched_at  TIMESTAMPTZ NOT NULL,
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

On every inbound `Message` / `Follow` webhook event, the relay calls `GET /v2/bot/profile/:userId` if the cached row is empty or older than 7 days, UPSERTs, and bumps `last_seen_at`. `/pair` response surfaces the cached profile so thClaws renders it on the sidebar pill.

Third-party relays MAY skip the profile cache — `/pair` response fields are optional. thClaws degrades gracefully.

## Surface-aware tools

A subtle gotcha for any relay's design: when a turn is driven by LINE, the user is **not at the local thClaws GUI**. Tools whose only output surface is the desktop modal (currently: `AskUserQuestion`) would hang the LINE conversation forever — the prompt lands on a screen the user can't see.

thClaws short-circuits `AskUserQuestion` on LINE-driven turns and returns a message instructing the model to fold the question into its LINE reply text. The user's next inbound LINE message becomes the answer naturally. See `crates/core/src/tools/ask.rs` `LINE_DRIVEN_TURN`.

Other surface-coupled tools are evaluated case-by-case as they're added. Relay implementers don't need to do anything — this is enforced on the client side.

## Permission gating

When the LINE bridge is connected, thClaws auto-switches `PermissionMode` to `LineGated` and routes all mutating-tool approval prompts to LINE as Quick Reply chips (`[✅ Approve] [🚫 Deny]`). Postbacks come back over the WS as `{ "kind": "postback", "data": "tool:allow:<id>" | "tool:deny:<id>" }`. On `LineDisconnect`, the previous local mode (Auto / Ask / Plan) is restored.

See [`permissions.md`](permissions.md) for `LineGated` and the broader approval-sink trait.

## Workspace-only

The official relay (`crates/line-server/`) is **server-side infrastructure** and never ships with the public thClaws release. `make sync-public` excludes it via `--exclude='line-server/'` in `Makefile`'s `RSYNC_CRATES_EXCLUDES`. Anyone self-hosting reimplements the protocol; the public surface is only the client-side `crates/core/src/line/` module and this doc.
