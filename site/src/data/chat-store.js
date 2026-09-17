import { api } from "./api";
import { pubsub } from "./pubsub";
import { chatSocket } from "./chat-socket";

const LAST_READ_KEY_PREFIX = "chat-last-read-message-id:";

// Normalizes the two wire shapes chat messages arrive in onto one client shape: the REST backfill
// (`GET /get-chat-messages`, server's `ChatMessage` model) uses `messageText`/`createdAt`; the
// live websocket frame (`WsEnvelope::ChatMessage`, server's `ChatMessagePayload`) uses `text` and
// carries no timestamp of its own (the envelope's own `ts` isn't threaded through by
// `chat-socket.js`, so this falls back to "now" - close enough for a live-arriving message).
function normalize(raw) {
  return {
    messageId: raw.messageId,
    memberName: raw.memberName ?? null,
    text: raw.text ?? raw.messageText ?? "",
    createdAt: raw.createdAt ?? new Date().toISOString(),
  };
}

// Backs the chat drawer: owns the ~200-message backfill + live socket merge, and tracks unread
// state two ways - a last-read message id in localStorage (instant, works offline, this browser
// only) mirrored to the server's read cursor via `POST /mark-chat-read` (spec §6). The localStorage
// write happens first and drives this tab's own badge immediately; the server call is fire-and-
// forget best-effort so a dropped request never blocks the local read state. Incoming
// `chat_read` frames (this account's *other* live sessions reading chat - see
// `chat-socket.js`) advance the same localStorage cursor here, so a stray page reload doesn't
// resurrect an unread badge another session already cleared.
class ChatStore {
  constructor() {
    this.enabled = false;
    this.messages = [];
    this.groupName = undefined;
    this.myMemberName = undefined;
    this.syncedReadCursor = 0;
  }

  enable(groupName) {
    if (this.enabled) return;
    this.enabled = true;
    this.groupName = groupName;
    this.messages = [];
    this.myMemberName = undefined;
    this.syncedReadCursor = 0;
    this.handleSocketMessage = this.handleSocketMessage.bind(this);
    this.handleSocketRead = this.handleSocketRead.bind(this);
    pubsub.subscribe("chat-socket-message", this.handleSocketMessage);
    pubsub.subscribe("chat-socket-read", this.handleSocketRead);
    chatSocket.enable();
    this.loadBackfill();
    this.resolveMyMemberName();
  }

  disable() {
    this.enabled = false;
    this.groupName = undefined;
    this.messages = [];
    chatSocket.disable();
    if (this.handleSocketMessage) pubsub.unsubscribe("chat-socket-message", this.handleSocketMessage);
    if (this.handleSocketRead) pubsub.unsubscribe("chat-socket-read", this.handleSocketRead);
    pubsub.unpublish("chat-messages");
    pubsub.unpublish("chat-unread-count");
  }

  // Resolved once per `enable()` via the same endpoint the group-settings page already uses to
  // know "who am I" - needed to tell whether an incoming `chat_read` frame is one of *this*
  // account's other sessions (see `handleSocketRead`) versus another group member's, since the
  // broadcast carries no account id (see server's `ChatReadPayload` doc comment).
  async resolveMyMemberName() {
    try {
      const response = await api.getMyPermissions();
      this.myMemberName = response.ok ? (await response.json()).member_name ?? null : null;
    } catch {
      this.myMemberName = null;
    }
  }

  async loadBackfill() {
    const backfill = await api.getChatMessages();
    if (!this.enabled) return;
    // Live frames can arrive while the backfill request is in flight - merge rather than
    // overwrite so nothing sent in that window is lost.
    const existingIds = new Set(this.messages.map((m) => m.messageId));
    const merged = [...backfill.map(normalize), ...this.messages.filter((m) => !existingIds.has(m.messageId))];
    merged.sort((a, b) => a.messageId - b.messageId);
    this.messages = merged;
    this.publishMessages();
    this.publishUnreadCount();
  }

  handleSocketMessage(payload) {
    this.addMessage(normalize(payload));
  }

  // Another of this account's sessions (plugin side panel, or another browser tab) advanced the
  // server read cursor - mirror it into this tab's localStorage cursor so the badge clears live,
  // matching spec §6's "clearing their dot/badge in real time".
  handleSocketRead(payload) {
    if (!payload || !payload.memberName) return;
    if (this.myMemberName === undefined || payload.memberName !== this.myMemberName) return;
    if (payload.messageId > this.lastReadMessageId()) {
      localStorage.setItem(this.lastReadKey(), String(payload.messageId));
      this.syncedReadCursor = Math.max(this.syncedReadCursor, payload.messageId);
    }
    this.publishUnreadCount();
  }

  // Optimistic local append for a message this tab just sent - see chat-drawer.js's composer,
  // which calls this with the raw `ChatMessage` JSON `send-chat-message` returns. Deduped (below)
  // against the same message arriving back over the socket a moment later.
  addFromRestResponse(raw) {
    this.addMessage(normalize(raw));
  }

  addMessage(message) {
    if (this.messages.some((m) => m.messageId === message.messageId)) return;
    this.messages.push(message);
    this.messages.sort((a, b) => a.messageId - b.messageId);
    this.publishMessages();
    this.publishUnreadCount();
  }

  publishMessages() {
    pubsub.publish("chat-messages", this.messages);
  }

  lastReadKey() {
    return `${LAST_READ_KEY_PREFIX}${this.groupName}`;
  }

  lastReadMessageId() {
    return Number(localStorage.getItem(this.lastReadKey()) || 0);
  }

  publishUnreadCount() {
    const lastRead = this.lastReadMessageId();
    const unread = this.messages.filter((m) => m.messageId > lastRead).length;
    pubsub.publish("chat-unread-count", unread);
  }

  // Called by chat-drawer.js only when the drawer is both visible and focused (spec §6) - this
  // itself does no visibility/focus checking, it just records "read up to the newest message".
  markRead() {
    const newest = this.messages.reduce((max, m) => Math.max(max, m.messageId), 0);
    if (newest > 0) {
      localStorage.setItem(this.lastReadKey(), String(newest));
      this.syncReadCursor(newest);
    }
    pubsub.publish("chat-unread-count", 0);
  }

  // Fire-and-forget push to the server cursor - skips a redundant call when this tab already
  // synced this exact (or a newer) message id, since `markRead` can be invoked repeatedly (every
  // new message while the drawer is open and focused).
  async syncReadCursor(messageId) {
    if (messageId <= this.syncedReadCursor) return;
    this.syncedReadCursor = messageId;
    try {
      await api.markChatRead(messageId);
    } catch {
      // best-effort - localStorage's optimistic write already drives this tab's own badge
    }
  }
}

const chatStore = new ChatStore();

export { chatStore };
