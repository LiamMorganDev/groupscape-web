import { BaseElement } from "../base-element/base-element";
import {
  activityEventDescription,
  activityMetaLabel,
  activityLinkFor,
  activityDisplayType,
  killGroupKey,
  KILL_MERGE_WINDOW_MS,
  subKillsMatch,
  delveLevelMatch,
} from "../data/activity-event-copy";
import { pubsub } from "../data/pubsub";

const LEAVE_ANIMATION_MS = 300;
const RELATIVE_TIME_TICK_MS = 1000;
// Unlike activity-event toasts (meant to sit around while the app runs unattended - see the class
// doc below), a ping is a live "look here now" callout - it auto-dismisses so the stack doesn't
// fill up with stale pings from a session left open a while.
const PING_TOAST_LIFETIME_MS = 20000;
// Activity-event toasts still auto-dismiss eventually so a long-unattended session doesn't build
// up a stack forever, but on a much longer timer than pings since they're meant to sit around.
const TOAST_LIFETIME_MS = 5 * 60 * 1000;

const TOAST_ICONS = {
  kill: "⚔",
  death: "☠",
  quest: "✓",
  diary: "⛰",
  "combat-achievement": "✦",
  collection_log: "❖",
  clue: "📜",
  raid: "🏆",
  level_up: "⬆",
  ping: "📍",
};

function relativeTime(fromDate) {
  const seconds = Math.floor((Date.now() - fromDate.getTime()) / 1000);
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

// Toasts are meant to sit in the corner while the app runs unattended, so they stay on screen
// for a long time (see `TOAST_LIFETIME_MS`) rather than auto-expiring after a few seconds -
// but they do eventually clear themselves out if left unattended long enough.
export class ToastStack extends BaseElement {
  constructor() {
    super();
  }

  html() {
    return `{{toast-stack.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    this.render();
    this.list = this.querySelector(".toast-stack__list");
    this.clearButton = this.querySelector(".toast-stack__clear");
    this.eventListener(this.clearButton, "click", () => this.clearAll());
    this.subscribe("toast", this.handleToast.bind(this));
    this.tickInterval = window.setInterval(this.tickTimestamps.bind(this), RELATIVE_TIME_TICK_MS);
    // key (kill/death|member|boss, see `groupKey` below) -> { toast, el } for the kill/death
    // toast currently on screen for that key, so a repeat one updates it in place instead of
    // stacking a duplicate.
    this.killToastGroups = new Map();
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    window.clearInterval(this.tickInterval);
  }

  // Kill and death groups are namespaced separately (rather than sharing `killGroupKey` raw) so a
  // kill and a death against the same NPC name don't collide in `killToastGroups`.
  groupKey(type, event) {
    return `${type}|${killGroupKey(event)}`;
  }

  // Repeat kills/deaths against the same boss by the same member fold into the toast still on
  // screen for that one (see `KILL_MERGE_WINDOW_MS`) rather than stacking a duplicate. Returns
  // true when the toast was merged in place, so the caller skips creating a new element.
  mergeIntoExistingToast(toast, displayType) {
    const key = this.groupKey(displayType, toast.event);
    const group = this.killToastGroups.get(key);
    if (!group) return false;
    // A combo kill (Barrows/Moons of Peril) whose sub_kills differ from the toast it would
    // otherwise fold into is a distinct kill, not a repeat - see `subKillsMatch`.
    if (displayType === "kill" && !subKillsMatch(group.toast.event.payload, toast.event.payload)) return false;
    // Doom of Mokhaiotl only: same reasoning, for a differing delve level - see `delveLevelMatch`.
    if (displayType === "kill" && !delveLevelMatch(group.toast.event.payload, toast.event.payload)) return false;
    const eventTime = new Date(toast.event.occurred_at).getTime();
    const groupTime = new Date(group.toast.event.occurred_at).getTime();
    if (Math.abs(eventTime - groupTime) > KILL_MERGE_WINDOW_MS) return false;

    group.toast.event = {
      ...group.toast.event,
      aggregateCount: (group.toast.event.aggregateCount || 1) + 1,
      occurred_at: eventTime > groupTime ? toast.event.occurred_at : group.toast.event.occurred_at,
    };
    group.el.querySelector(".toast-stack__title").innerHTML = this.titleHtml(group.toast);
    group.el.dataset.occurredAt = group.toast.event.occurred_at;
    this.updateTimestamp(group.el);
    group.el.classList.remove("toast-stack__toast--pulse");
    void group.el.offsetWidth;
    group.el.classList.add("toast-stack__toast--pulse");
    this.startLifetimeTimer(group.el);
    return true;
  }

  handleToast(toast) {
    const displayType = toast.event && activityDisplayType(toast.event);
    // A death ends whatever kill streak was building for that member - the next kill of the same
    // boss should spawn a fresh toast rather than folding into the one from before the death.
    if (displayType === "death") {
      for (const key of this.killToastGroups.keys()) {
        if (key.startsWith(`kill|${toast.event.member_name}|`)) this.killToastGroups.delete(key);
      }
    }

    const isGroupableToast = displayType === "kill" || displayType === "death";
    if (isGroupableToast && this.mergeIntoExistingToast(toast, displayType)) return;

    const el = document.createElement("a");
    el.className = `toast-stack__toast toast-stack__toast--${toast.type}`;
    el.href = toast.type === "ping" ? "/group/map" : toast.event ? activityLinkFor(toast.event) : "/group/activity";
    el.dataset.occurredAt = toast.event?.occurred_at || new Date().toISOString();
    // Clue tiers share one "clue" toast type but each gets its own color, matching the activity
    // feed's per-tier treatment (see activity-feed-event.js).
    if (toast.type === "clue" && toast.event?.payload?.clueTier) {
      el.style.setProperty("--toast-color", `var(--clue-${toast.event.payload.clueTier})`);
    }
    el.innerHTML = `
      <div class="toast-stack__content">
        <div class="toast-stack__icon">${TOAST_ICONS[toast.type] || "•"}</div>
        <div class="toast-stack__body">
          <div class="toast-stack__title">${this.titleHtml(toast)}</div>
          <div class="toast-stack__meta">
            ${
              this.metaHtml(toast)
                ? `<span>${this.metaHtml(toast)}</span><span class="toast-stack__dot">&middot;</span>`
                : ""
            }
            <span class="toast-stack__time"></span>
          </div>
        </div>
      </div>
      <div class="toast-stack__bar"><div class="toast-stack__bar-fill"></div></div>
      <button class="toast-stack__close" type="button" aria-label="Dismiss">&times;</button>
    `;
    this.eventListener(
      el.querySelector(".toast-stack__close"),
      "click",
      (event) => {
        event.preventDefault();
        event.stopPropagation();
        this.dismiss(el);
      },
      { passive: false }
    );
    this.eventListener(
      el,
      "click",
      (event) => {
        event.preventDefault();
        if (toast.type === "ping" && toast.ping) {
          // canvas-map.js replays this on connect (pubsub's "most recent" behavior), so it still
          // works even when the map page isn't mounted yet at click time.
          pubsub.publish("jump-to-ping", { x: toast.ping.x, y: toast.ping.y, plane: toast.ping.plane });
        }
        window.history.pushState("", "", el.href);
        this.dismiss(el);
      },
      { passive: false }
    );
    this.updateTimestamp(el);
    this.list.appendChild(el);
    this.updateClearButton();

    if (isGroupableToast) {
      const key = this.groupKey(displayType, toast.event);
      el.dataset.killGroupKey = key;
      this.killToastGroups.set(key, { toast, el });
    }

    if (toast.type === "ping") {
      setTimeout(() => this.dismiss(el), PING_TOAST_LIFETIME_MS);
    } else {
      this.startLifetimeTimer(el);
    }
  }

  // (Re)starts the long auto-dismiss timer and snaps the depleting bar back to full. Called both
  // for a freshly-created toast and for a kill toast that got merged into in place, so a live
  // kill streak keeps refreshing its toast's lifetime instead of it expiring mid-streak.
  startLifetimeTimer(el) {
    if (el._lifetimeTimeout) window.clearTimeout(el._lifetimeTimeout);
    el.dataset.lifetimeStart = Date.now();
    el._lifetimeTimeout = window.setTimeout(() => this.dismiss(el), TOAST_LIFETIME_MS);
    this.updateLifetimeBar(el);
  }

  updateLifetimeBar(el) {
    const fill = el.querySelector(".toast-stack__bar-fill");
    if (!fill || !el.dataset.lifetimeStart) return;
    const elapsed = Date.now() - Number(el.dataset.lifetimeStart);
    const remaining = Math.max(0, 1 - elapsed / TOAST_LIFETIME_MS);
    fill.style.width = `${remaining * 100}%`;
  }

  dismiss(el) {
    if (el._lifetimeTimeout) window.clearTimeout(el._lifetimeTimeout);
    el.classList.add("toast-stack__toast--leaving");
    el.addEventListener("animationend", () => el.remove());
    // Fallback in case prefers-reduced-motion skips the leave animation entirely.
    setTimeout(() => {
      el.remove();
      this.updateClearButton();
    }, LEAVE_ANIMATION_MS);
    // Dismissed toasts no longer accept a merge - the next kill for this key spawns a fresh one.
    if (el.dataset.killGroupKey && this.killToastGroups.get(el.dataset.killGroupKey)?.el === el) {
      this.killToastGroups.delete(el.dataset.killGroupKey);
    }
  }

  clearAll() {
    this.list.querySelectorAll(".toast-stack__toast:not(.toast-stack__toast--leaving)").forEach((el) => {
      this.dismiss(el);
    });
    this.updateClearButton();
  }

  updateClearButton() {
    this.clearButton.hidden = !this.list.querySelector(".toast-stack__toast:not(.toast-stack__toast--leaving)");
  }

  tickTimestamps() {
    this.list.querySelectorAll(".toast-stack__toast").forEach((el) => {
      this.updateTimestamp(el);
      this.updateLifetimeBar(el);
    });
  }

  updateTimestamp(el) {
    const timeEl = el.querySelector(".toast-stack__time");
    if (timeEl) timeEl.textContent = relativeTime(new Date(el.dataset.occurredAt));
  }

  subject(name) {
    return `<span class="toast-stack__subject">${name}</span>`;
  }

  // Copy is shared with the activity feed (see `data/activity-event-copy.js`) - the toast just
  // highlights the member name instead of the subject.
  titleHtml(toast) {
    if (toast.type === "ping" && toast.ping) {
      const where = toast.ping.npcName ? this.subject(toast.ping.npcName) : "a location";
      return `${this.subject(toast.ping.memberName)} pinged ${where}`;
    }
    if (!toast.event) return this.subject(toast.memberName || "");
    return activityEventDescription(toast.event, { member: (name) => this.subject(name) });
  }

  metaHtml(toast) {
    if (toast.type === "ping") return "Click to view on map";
    if (!toast.event) return "";
    return activityMetaLabel(toast.event);
  }
}

customElements.define("toast-stack", ToastStack);
