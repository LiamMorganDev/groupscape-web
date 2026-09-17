import { BaseElement } from "../base-element/base-element";
import { api } from "../data/api";
import { chatStore } from "../data/chat-store";
import { groupData } from "../data/group-data";
import { adminViewSession } from "../data/admin-view-session";

function escapeHtml(value) {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// `<mark>` around every case-insensitive match of `query` in `text` - both sides already
// HTML-escaped first so this never reintroduces markup from the message itself.
function highlight(escapedText, query) {
  if (!query) return escapedText;
  const pattern = new RegExp(escapeRegExp(escapeHtml(query)), "gi");
  return escapedText.replace(pattern, (match) => `<mark>${match}</mark>`);
}

function memberColor(memberName) {
  if (!memberName) return "var(--primary-text)";
  const hue = groupData.members.get(memberName)?.hue;
  if (hue === undefined) return "var(--primary-text)";
  return `hsl(${hue}, 75%, 68%)`;
}

function formatTime(isoString) {
  const date = new Date(isoString);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// Chrome/Firefox/Safari all support setting a data: URI as a favicon `<link>`'s href at runtime -
// drawing a small red dot onto the existing icon is the standard "you have unread notifications"
// pattern for a backgrounded tab, paired with a "(N) " title prefix below.
function buildBadgedFavicon(baseHref, onReady) {
  const image = new Image();
  image.onload = () => {
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.drawImage(image, 0, 0);
    const radius = image.width * 0.28;
    ctx.beginPath();
    ctx.arc(image.width - radius, radius, radius, 0, Math.PI * 2);
    ctx.fillStyle = "#ff4b4b";
    ctx.strokeStyle = "#000000";
    ctx.lineWidth = Math.max(1, image.width * 0.06);
    ctx.fill();
    ctx.stroke();
    onReady(canvas.toDataURL("image/png"));
  };
  image.src = baseHref;
}

/**
 * The webapp's group chat panel (spec §9): a floating bubble (persistent across every page, not
 * a dedicated nav tab - see the approved chat-panel design) that expands into a drawer showing
 * the same ~200-message backfill + live feed as the RuneLite side-panel Chat tab. Search filters
 * that already-loaded set client-side (no server-side search endpoint - out of scope per spec
 * §9). Unread state is inferred client-side (`chat-store.js`) since the server has no read-cursor
 * yet (spec §6, still open) - a badge dot on the bubble, plus a favicon badge + title prefix
 * while the tab is hidden.
 */
export class ChatDrawer extends BaseElement {
  constructor() {
    super();
    this.searchQuery = "";
    this.messages = [];
    this.faviconBadged = false;
  }

  html() {
    return `{{chat-drawer.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    // Chat is a real-member feature - `chatStore` is never enabled in admin-view mode (see
    // `app-initializer.js`'s `loadAdminView`, and `chatSocketUrl`'s own admin-view guard), and
    // there's no group token to authenticate a send with anyway, so don't show the bubble at all
    // rather than rendering a drawer that can only ever be empty.
    if (adminViewSession.get()) {
      this.hidden = true;
      return;
    }

    this.render();

    this.bubble = this.querySelector(".chat-drawer__bubble");
    this.badge = this.querySelector(".chat-drawer__badge");
    this.panel = this.querySelector(".chat-drawer__panel");
    this.closeButton = this.querySelector(".chat-drawer__close");
    this.searchInput = this.querySelector(".chat-drawer__search");
    this.list = this.querySelector(".chat-drawer__list");
    this.emptyState = this.querySelector(".chat-drawer__empty");
    this.errorEl = this.querySelector(".chat-drawer__error");
    this.composer = this.querySelector(".chat-drawer__composer");
    this.composerInput = this.querySelector(".chat-drawer__composer-input");
    this.composerSend = this.querySelector(".chat-drawer__composer-send");

    this.cacheFavicons();

    this.eventListener(this.bubble, "click", () => this.open());
    this.eventListener(this.closeButton, "click", () => this.close());
    this.eventListener(this.searchInput, "input", () => {
      this.searchQuery = this.searchInput.value;
      this.renderMessages();
    });
    this.eventListener(this.composer, "submit", (event) => this.handleSend(event), { passive: false });
    this.eventListener(document, "visibilitychange", () => {
      if (!document.hidden) this.clearFaviconBadge();
    });

    this.subscribe("chat-messages", (messages) => {
      this.messages = messages;
      this.renderMessages();
    });
    this.subscribe("chat-unread-count", (count) => this.updateUnread(count));
  }

  cacheFavicons() {
    this.faviconLinks = Array.from(document.querySelectorAll('link[rel="icon"]')).map((link) => ({
      link,
      originalHref: link.href,
    }));
    this.originalTitle = document.title;
  }

  open() {
    this.panel.hidden = false;
    this.bubble.hidden = true;
    this.searchInput.focus();
    chatStore.markRead();
    this.clearFaviconBadge();
  }

  close() {
    this.panel.hidden = true;
    this.bubble.hidden = false;
  }

  updateUnread(count) {
    this.badge.hidden = count === 0;
    this.badge.textContent = count > 99 ? "99+" : String(count);
    if (count > 0 && document.hidden) this.setFaviconBadge(count);
  }

  setFaviconBadge(count) {
    document.title = `(${count > 99 ? "99+" : count}) ${this.originalTitle}`;
    if (this.faviconBadged) return;
    this.faviconBadged = true;
    for (const { link, originalHref } of this.faviconLinks) {
      buildBadgedFavicon(originalHref, (dataUrl) => {
        link.href = dataUrl;
      });
    }
  }

  clearFaviconBadge() {
    document.title = this.originalTitle;
    if (!this.faviconBadged) return;
    this.faviconBadged = false;
    for (const { link, originalHref } of this.faviconLinks) {
      link.href = originalHref;
    }
  }

  filteredMessages() {
    const query = this.searchQuery.trim().toLowerCase();
    if (!query) return this.messages;
    return this.messages.filter(
      (m) => m.text.toLowerCase().includes(query) || (m.memberName || "").toLowerCase().includes(query)
    );
  }

  renderMessages() {
    const filtered = this.filteredMessages();
    this.emptyState.hidden = filtered.length > 0;
    const wasScrolledToBottom = this.list.scrollHeight - this.list.scrollTop - this.list.clientHeight < 40;

    this.list.innerHTML = filtered
      .map((m) => {
        const name = m.memberName ? escapeHtml(m.memberName) : "System";
        const text = highlight(escapeHtml(m.text), this.searchQuery.trim());
        return `
          <div class="chat-drawer__message">
            <span class="chat-drawer__message-time">${formatTime(m.createdAt)}</span>
            <span class="chat-drawer__message-name" style="color: ${memberColor(m.memberName)}">${name}:</span>
            <span class="chat-drawer__message-text">${text}</span>
          </div>
        `;
      })
      .join("");

    if (wasScrolledToBottom) this.list.scrollTop = this.list.scrollHeight;
  }

  async handleSend(event) {
    event.preventDefault();
    const text = this.composerInput.value.trim();
    if (!text) return;

    this.errorEl.hidden = true;
    this.composerSend.disabled = true;
    try {
      const response = await api.sendChatMessage(text);
      if (!response.ok) {
        const message = await response.text().catch(() => "Failed to send message");
        this.showError(message || "Failed to send message");
        return;
      }
      const created = await response.json();
      chatStore.addFromRestResponse(created);
      this.composerInput.value = "";
    } catch {
      this.showError("Failed to send message");
    } finally {
      this.composerSend.disabled = false;
    }
  }

  showError(message) {
    this.errorEl.textContent = message;
    this.errorEl.hidden = false;
  }
}

customElements.define("chat-drawer", ChatDrawer);
