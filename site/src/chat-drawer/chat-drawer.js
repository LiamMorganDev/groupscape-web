import { BaseElement } from "../base-element/base-element";
import { api } from "../data/api";
import { chatStore } from "../data/chat-store";
import { groupData } from "../data/group-data";
import { adminViewSession } from "../data/admin-view-session";
import { confirmDialogManager } from "../confirm-dialog/confirm-dialog-manager";

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
 * §9). A badge dot on the bubble, plus a favicon badge + title prefix while the tab is hidden.
 *
 * Read-cursor auto-advance (spec §6) only fires while the drawer is both open (visible) and this
 * browser tab has OS focus - `maybeMarkRead` is the single gate for that rule, called from every
 * place the visible/focused state could have just become true (open, window focus, tab becoming
 * visible, a new message arriving while already open+focused).
 */
export class ChatDrawer extends BaseElement {
  constructor() {
    super();
    this.searchQuery = "";
    this.messages = [];
    this.faviconBadged = false;
    this.isAdmin = false;
    // Snapshot of {cursor, newestId} taken once per open() (see `freezeDividerSnapshot`) - null
    // while closed. Bounds which messages the divider logic in `renderMessages` will ever
    // consider, so a message that arrives *after* opening (sent by this tab, or live from
    // someone else while actively watching) never gets flagged unread and flashes a "New"
    // divider above it - it only ever marks the backlog that existed at open time.
    this.dividerSnapshot = null;
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
    this.clearAllButton = this.querySelector(".chat-drawer__clear-all");
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
    this.eventListener(this.clearAllButton, "click", () => this.handleClearAll());
    this.eventListener(this.searchInput, "input", () => {
      this.searchQuery = this.searchInput.value;
      this.renderMessages();
    });
    this.eventListener(this.composer, "submit", (event) => this.handleSend(event), { passive: false });
    this.eventListener(this.list, "click", (event) => this.handleListClick(event));
    this.eventListener(document, "visibilitychange", () => {
      if (!document.hidden) {
        this.clearFaviconBadge();
        this.maybeMarkRead();
      }
    });
    this.eventListener(window, "focus", () => this.maybeMarkRead());

    this.subscribe("chat-messages", (messages) => {
      this.messages = messages;
      this.renderMessages();
      this.maybeMarkRead();
    });
    this.subscribe("chat-unread-count", (count) => this.updateUnread(count));
    this.subscribe("chat-is-admin", (isAdmin) => {
      this.isAdmin = isAdmin;
      this.clearAllButton.hidden = !isAdmin;
      this.renderMessages();
    });
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
    this.clearFaviconBadge();
    this.freezeDividerSnapshot();
    this.maybeMarkRead();
  }

  close() {
    this.panel.hidden = true;
    this.bubble.hidden = false;
    this.dividerSnapshot = null;
  }

  // Captured *before* `maybeMarkRead()` (called right after this, in `open()`) can advance the
  // read cursor - so it reflects "what was unread the instant this viewing session started",
  // not "what's still unread now". A no-op past the first call this session (drawer stays open
  // across repeated re-renders) - `close()` is what lets the next `open()` take a fresh snapshot.
  freezeDividerSnapshot() {
    if (this.dividerSnapshot !== null) return;
    this.dividerSnapshot = {
      cursor: chatStore.lastReadMessageId(),
      newestId: this.messages.reduce((max, m) => Math.max(max, m.messageId), 0),
    };
  }

  // Visible = drawer panel open; focused = this browser tab has OS focus (Page Visibility API /
  // `document.hasFocus()`) - selected-but-unfocused deliberately doesn't advance the cursor (spec
  // §6). `document.hasFocus()` alone would count a background tab that's simply not hidden yet
  // (e.g. mid-transition), so both checks apply.
  maybeMarkRead() {
    if (!this.panel || this.panel.hidden) return;
    if (document.hidden || !document.hasFocus()) return;
    chatStore.markRead();
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

  // Divider marks the boundary between messages already read before this drawer was last opened
  // and whatever was unread at that moment - bounded above by `dividerSnapshot.newestId` (see
  // `freezeDividerSnapshot`) so a message that arrives *while* open (this tab's own send
  // included) never itself gets flagged unread. While closed, there's no frozen snapshot yet, so
  // this falls back to the live cursor with no upper bound - matches what the *next* open() would
  // freeze anyway, since nothing here is visible to flicker. Suppressed for a cursor of 0 (never
  // read anything in this group yet - nothing "already read" to draw a boundary under) and when
  // nothing in range is unread.
  renderMessages() {
    const filtered = this.filteredMessages();
    this.emptyState.hidden = filtered.length > 0;
    const wasScrolledToBottom = this.list.scrollHeight - this.list.scrollTop - this.list.clientHeight < 40;

    const lastRead = this.dividerSnapshot ? this.dividerSnapshot.cursor : chatStore.lastReadMessageId();
    const newestInRange = this.dividerSnapshot ? this.dividerSnapshot.newestId : Infinity;
    const firstUnreadIndex = filtered.findIndex((m) => m.messageId > lastRead && m.messageId <= newestInRange);
    const showDivider = lastRead > 0 && firstUnreadIndex > 0;

    this.list.innerHTML = filtered
      .map((m, index) => {
        const name = m.memberName ? escapeHtml(m.memberName) : "System";
        const icon =
          m.memberName && groupData.members.has(m.memberName)
            ? `<player-icon player-name="${escapeHtml(m.memberName)}"></player-icon>`
            : "";
        const text = highlight(escapeHtml(m.text), this.searchQuery.trim());
        const rowClass = this.isAdmin ? "chat-drawer__message chat-drawer__message--admin" : "chat-drawer__message";
        const deleteButton = this.isAdmin
          ? `<button class="chat-drawer__message-delete" type="button" data-message-id="${m.messageId}" aria-label="Delete message">&times;</button>`
          : "";
        const divider =
          index === firstUnreadIndex && showDivider
            ? `<div class="chat-drawer__unread-divider"><span>New</span></div>`
            : "";
        return `
          ${divider}
          <div class="${rowClass}">
            <span class="chat-drawer__message-time">${formatTime(m.createdAt)}</span>
            <span class="chat-drawer__message-name" style="color: ${memberColor(m.memberName)}">${icon}${name}:</span>
            <span class="chat-drawer__message-text">${text}</span>
            ${deleteButton}
          </div>
        `;
      })
      .join("");

    if (wasScrolledToBottom) this.list.scrollTop = this.list.scrollHeight;
  }

  handleListClick(event) {
    const deleteButton = event.target.closest(".chat-drawer__message-delete");
    if (!deleteButton) return;
    const messageId = Number(deleteButton.dataset.messageId);
    if (!Number.isFinite(messageId)) return;
    confirmDialogManager.confirm({
      headline: "Delete this message?",
      body: "This removes it for every member of the group.",
      yesCallback: () => this.deleteMessage(messageId),
      noCallback: () => {},
    });
  }

  async deleteMessage(messageId) {
    this.errorEl.hidden = true;
    try {
      const response = await chatStore.deleteMessage(messageId);
      if (!response.ok) {
        const message = await response.text().catch(() => "Failed to delete message");
        this.showError(message || "Failed to delete message");
      }
    } catch {
      this.showError("Failed to delete message");
    }
  }

  handleClearAll() {
    confirmDialogManager.confirm({
      headline: "Clear the entire group chat?",
      body: "This permanently deletes every message for every member of the group.",
      yesCallback: () => this.deleteAllMessages(),
      noCallback: () => {},
    });
  }

  async deleteAllMessages() {
    this.errorEl.hidden = true;
    try {
      const response = await chatStore.deleteAllMessages();
      if (!response.ok) {
        const message = await response.text().catch(() => "Failed to clear chat");
        this.showError(message || "Failed to clear chat");
      }
    } catch {
      this.showError("Failed to clear chat");
    }
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
