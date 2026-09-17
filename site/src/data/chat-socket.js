import { api } from "./api";
import { pubsub } from "./pubsub";

// Reconnect backoff for a dropped chat socket (network blip, server restart) - fixed steps rather
// than exponential since a chat drawer reconnecting within a few seconds matters more than being
// gentle on the server, and `ChatRateLimiter`/connection limits aren't a concern for a single
// per-tab socket.
const RECONNECT_DELAYS_MS = [1000, 2000, 5000, 10000];

// Owns the webapp's one WebSocket connection to `/api/group/{group_name}/ws` (see `api.js`'s
// `chatSocketUrl`) for the chat drawer's live feed. The same endpoint also sends
// `roster_snapshot`/`vitals_update`/etc. frames (it's shared with the RuneLite party overlay -
// see server's `party_overlay_ws`), but this client only republishes the `chat_message`,
// `chat_rate_limited`, `chat_read`, `chat_message_deleted`, and `chat_messages_cleared` envelope
// types onto pubsub; everything else is ignored since the webapp already gets roster/vitals data
// from its own poll loop.
class ChatSocket {
  constructor() {
    this.enabled = false;
    this.socket = undefined;
    this.reconnectAttempt = 0;
    this.reconnectTimeout = undefined;
  }

  enable() {
    if (this.enabled) return;
    this.enabled = true;
    this.reconnectAttempt = 0;
    this.connect();
  }

  disable() {
    this.enabled = false;
    if (this.reconnectTimeout) window.clearTimeout(this.reconnectTimeout);
    this.reconnectTimeout = undefined;
    if (this.socket) {
      // Drop onclose's reconnect-on-close behavior for a socket we're closing ourselves.
      this.socket.onclose = null;
      this.socket.close();
      this.socket = undefined;
    }
  }

  connect() {
    if (!this.enabled) return;
    const url = api.chatSocketUrl;
    if (!url) return;

    const socket = new WebSocket(url);
    this.socket = socket;

    socket.onopen = () => {
      this.reconnectAttempt = 0;
    };

    socket.onmessage = (event) => {
      let envelope;
      try {
        envelope = JSON.parse(event.data);
      } catch {
        return;
      }
      if (envelope.type === "chat_message") {
        pubsub.publish("chat-socket-message", envelope.payload);
      } else if (envelope.type === "chat_rate_limited") {
        pubsub.publish("chat-socket-rate-limited", envelope.payload);
      } else if (envelope.type === "chat_read") {
        pubsub.publish("chat-socket-read", envelope.payload);
      } else if (envelope.type === "chat_message_deleted") {
        pubsub.publish("chat-socket-message-deleted", envelope.payload);
      } else if (envelope.type === "chat_messages_cleared") {
        pubsub.publish("chat-socket-messages-cleared");
      }
    };

    socket.onclose = () => {
      if (!this.enabled || this.socket !== socket) return;
      const delay = RECONNECT_DELAYS_MS[Math.min(this.reconnectAttempt, RECONNECT_DELAYS_MS.length - 1)];
      this.reconnectAttempt += 1;
      this.reconnectTimeout = window.setTimeout(() => this.connect(), delay);
    };
  }
}

const chatSocket = new ChatSocket();

export { chatSocket };
