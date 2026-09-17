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

// Backs the chat drawer: owns the ~200-message backfill + live socket merge, and infers an unread
// count client-side from a last-read message id in localStorage - there's no server read-cursor
// yet (spec §6 "History, backfill, and read state" ticket is still open), so this doesn't sync
// across devices/tabs, only within this browser.
class ChatStore {
  constructor() {
    this.enabled = false;
    this.messages = [];
    this.groupName = undefined;
  }

  enable(groupName) {
    if (this.enabled) return;
    this.enabled = true;
    this.groupName = groupName;
    this.messages = [];
    this.handleSocketMessage = this.handleSocketMessage.bind(this);
    pubsub.subscribe("chat-socket-message", this.handleSocketMessage);
    chatSocket.enable();
    this.loadBackfill();
  }

  disable() {
    this.enabled = false;
    this.groupName = undefined;
    this.messages = [];
    chatSocket.disable();
    if (this.handleSocketMessage) pubsub.unsubscribe("chat-socket-message", this.handleSocketMessage);
    pubsub.unpublish("chat-messages");
    pubsub.unpublish("chat-unread-count");
  }

  async loadBackfill() {
    const backfill = await api.getChatMessages(0);
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

  markRead() {
    const newest = this.messages.reduce((max, m) => Math.max(max, m.messageId), 0);
    if (newest > 0) localStorage.setItem(this.lastReadKey(), String(newest));
    pubsub.publish("chat-unread-count", 0);
  }
}

const chatStore = new ChatStore();

export { chatStore };
