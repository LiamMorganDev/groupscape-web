import { BaseElement } from "../base-element/base-element";
import { api } from "../data/api";
import { adminApi } from "../data/admin-api";
import { adminViewSession } from "../data/admin-view-session";
import { confirmDialogManager } from "../confirm-dialog/confirm-dialog-manager";
import { pubsub } from "../data/pubsub";
import { utility } from "../utility";
import {
  activityDisplayType,
  killGroupKey,
  KILL_MERGE_WINDOW_MS,
  subKillsMatch,
  delveLevelMatch,
} from "../data/activity-event-copy";

const EVENT_TYPES = [
  [null, "All"],
  ["kill", "Kills"],
  ["death", "Deaths"],
  ["quest", "Quests"],
  ["diary", "Diaries"],
  ["combat_task", "Combat tasks"],
  ["collection_log", "Collection log"],
  ["clue", "Clues"],
  ["raid", "Raids"],
  ["level_up", "Levels"],
];

const PAGE_LIMIT = 25;
const COUNT_LIMIT = 200;
const REFRESH_INTERVAL_MS = 15000;
// A page of raw kill events can collapse to just one new visible row when the rest merge into it
// (see `mergeOrCreateRow`) - a long, roughly-continuous same-boss farming history does this for
// page after page (each ~hour-long chunk of kills becomes one more row), so watching for a
// *zero*-growth streak never catches it: there's always some trickle of progress, just far too
// little to justify silently fetching hundreds of pages back-to-back. Cap how many pages the
// sentinel is allowed to auto-fetch in a row regardless of how much they added, and require a
// manual click to keep going past that - bounds the worst case to a fixed, fast amount of work.
const AUTO_LOAD_BURST_LIMIT = 4;
// Unconditional backstop on the sentinel auto-load loop, independent of autoLoadPaused/exhausted -
// guarantees the loop can never run away even if that state is ever wrong.
const AUTO_LOAD_HARD_CAP = 20;

export class ActivityFeedPage extends BaseElement {
  constructor() {
    super();
    this.disposed = false;
    this.members = [];
    this.selectedMember = null;
    this.selectedType = null;
    this.loaded = [];
    this.typeCountEvents = [];
    this.memberCountEvents = [];
    this.loadingMore = false;
    this.exhausted = false;
    this.autoLoadStreak = 0;
    this.autoLoadPaused = false;
    // key (member+boss, see `killGroupKey`) -> { event, row } for the kill currently shown as the
    // aggregated row for that key, so a repeat kill can be folded in rather than adding a row.
    this.feedGroups = new Map();
    // Only a site admin viewing this group read-only (see `admin-view-session.js`) can moderate
    // the feed - a real member never sees the select/delete controls.
    this.isAdmin = false;
    this.selecting = false;
    this.selectedIds = new Set();
    // Group-wide likes/comments toggle (see server's `activity_reactions_enabled`) - loaded once
    // per resetAndLoad, since it doesn't vary per-event.
    this.reactionsEnabled = false;
  }

  html() {
    return `{{activity-feed-page.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    // app-route reuses the same page instance across navigations (only constructs it once - see
    // app-route.js's `enable()`), so disconnectedCallback's `disposed = true` would otherwise stick
    // forever after the first time this page is navigated away from, silently no-op'ing every
    // loadMore/loadCounts/poll call on every future visit.
    this.disposed = false;
    this.render();

    this.rail = this.querySelector(".activity-feed-page__rail");
    this.memberFilters = this.querySelector(".activity-feed-page__member-filters");
    this.list = this.querySelector(".activity-feed-page__list");
    this.sentinel = this.querySelector(".activity-feed-page__sentinel");
    this.empty = this.querySelector(".activity-feed-page__empty");
    this.loadMoreButton = this.querySelector(".activity-feed-page__load-more");
    this.eventListener(this.loadMoreButton, "click", () => this.loadMore({ manual: true }));
    this.scrollContainer = this.closest(".authed-section__main-content") || document.documentElement;

    // Moderation controls (select rows, delete for everyone) are gated to a site admin's
    // read-only group-view session (see `admin-view-session.js`) - never available to a regular
    // member, including a group's own owner/admin.
    const adminView = adminViewSession.get();
    this.isAdmin = Boolean(adminView);
    this.adminGroupId = adminView?.groupId;
    this.selectToggle = this.querySelector(".activity-feed-page__select-toggle");
    this.modBar = this.querySelector(".activity-feed-page__mod-bar");
    this.modCount = this.querySelector(".activity-feed-page__mod-count");
    this.modDeleteButton = this.querySelector(".activity-feed-page__mod-delete");
    if (this.isAdmin) {
      // `adminApi` (the /api/admin-scoped client used by the admin panel) is separate from the
      // `api` singleton this page normally talks to - the admin-view session only wires the
      // latter (see app-initializer.js's `loadAdminView`), so seed `adminApi`'s bearer token here
      // before the moderation delete call needs it.
      adminApi.setCredentials(adminView.adminToken);
      this.selectToggle.hidden = false;
      this.eventListener(this.selectToggle, "click", () => this.setSelecting(!this.selecting));
      this.eventListener(this.list, "click", this.handleRowClick.bind(this));
      this.eventListener(this.querySelector(".activity-feed-page__mod-cancel"), "click", () =>
        this.setSelecting(false)
      );
      this.eventListener(this.modDeleteButton, "click", () => this.confirmDeleteSelected());
    }

    this.subscribe("members-updated", this.handleUpdatedMembers.bind(this));
    const [mostRecentMembers] = pubsub.getMostRecent("members-updated") || [];
    if (mostRecentMembers) {
      this.handleUpdatedMembers(mostRecentMembers);
    }

    this.renderRail();
    // On a hard refresh landing directly on this page, this component's connectedCallback can run
    // before app-initializer's api.enable() finishes setting the group credentials - loading then
    // instead of waiting for it fired requests as `/api/group/undefined/...`, which read as an
    // empty page with no way to retry. Wait for the first successful group-data fetch (the same
    // signal app-initializer itself waits on) before loading if the group isn't ready yet.
    if (api.groupName) {
      this.resetAndLoad();
    } else {
      this.subscribeOnce("get-group-data", () => this.resetAndLoad());
    }

    this.intersectionObserver = new IntersectionObserver(this.handleSentinelIntersect.bind(this), {
      root: this.scrollContainer,
      rootMargin: "120px",
    });
    this.intersectionObserver.observe(this.sentinel);

    this.refreshInterval = utility.callOnInterval(this.poll.bind(this), REFRESH_INTERVAL_MS, false);
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    // Stops any in-flight loadMore/poll/auto-load-loop from touching state or firing further
    // requests once this component is torn down - e.g. the group session getting invalidated
    // mid-request (see api.js's disable()-on-401) used to leave a stray autoLoadWhileVisible loop
    // running against a detached component with no group credentials, hammering the API with
    // `/api/group/undefined/...` requests that looked like a stuck "Load more".
    this.disposed = true;
    if (this.refreshInterval) window.clearInterval(this.refreshInterval);
    if (this.intersectionObserver) this.intersectionObserver.disconnect();
  }

  handleUpdatedMembers(members) {
    this.members = members.filter((member) => member.name !== "@SHARED");
    this.renderMemberFilters();
  }

  // IntersectionObserver only calls back on a threshold crossing, not continuously while
  // intersecting - if a page load doesn't move the sentinel, the observer never fires again.
  // A tight `while` loop chaining `await`s on quick-resolving promises never actually yields to
  // the browser's render/input loop (recursive microtasks starve it even though each step is
  // "async"), which is how a past version of this froze the tab. Pace each step through
  // requestAnimationFrame instead, and cap the run length unconditionally as a backstop in case
  // autoLoadPaused/exhausted are ever wrong.
  handleSentinelIntersect(entries) {
    if (this.autoLoadPaused) return;
    if (entries.some((entry) => entry.isIntersecting)) this.autoLoadWhileVisible();
  }

  isSentinelInView() {
    const sentinelRect = this.sentinel.getBoundingClientRect();
    const containerRect =
      this.scrollContainer === document.documentElement
        ? { top: 0, bottom: window.innerHeight }
        : this.scrollContainer.getBoundingClientRect();
    return sentinelRect.top < containerRect.bottom + 120 && sentinelRect.bottom > containerRect.top - 120;
  }

  autoLoadWhileVisible() {
    if (this.autoLoading) return;
    this.autoLoading = true;
    this.autoLoadRunLength = 0;
    const step = async () => {
      if (
        this.disposed ||
        this.autoLoadPaused ||
        this.exhausted ||
        this.autoLoadRunLength >= AUTO_LOAD_HARD_CAP ||
        !this.isSentinelInView()
      ) {
        this.autoLoading = false;
        return;
      }
      this.autoLoadRunLength++;
      await this.loadMore();
      if (this.disposed) return;
      requestAnimationFrame(step);
    };
    step();
  }

  // Best-effort counts, not true totals — there's no cheap way to get an exact total from a
  // cursor-paginated feed without a dedicated count query. Counted from a pool fetched ignoring
  // the axis being counted (but still respecting the *other* axis' filter), so picking a type
  // doesn't zero out every other type's count and vice versa.
  loadedCountForMember(memberName) {
    return this.memberCountEvents.filter((event) => !memberName || event.member_name === memberName).length;
  }

  loadedCountForType(type) {
    return this.typeCountEvents.filter((event) => !type || event.event_type === type).length;
  }

  renderRail() {
    if (!this.rail) return;
    this.rail.innerHTML = "";
    for (const [type, label] of EVENT_TYPES) {
      this.rail.appendChild(this.createTypeRailButton(type, label, this.loadedCountForType(type)));
    }
  }

  createTypeRailButton(type, label, count) {
    const button = document.createElement("button");
    button.className = "activity-feed-page__rail-btn";
    if (type) button.classList.add(`activity-feed-page__rail-btn--${type}`);
    button.classList.toggle("activity-feed-page__rail-btn--active", this.selectedType === type);
    button.innerHTML = `
      <span class="activity-feed-page__rail-label">${label}</span>
      <span class="activity-feed-page__rail-count">${count}</span>
    `;
    this.eventListener(button, "click", () => {
      if (this.selectedType === type) return;
      this.selectedType = type;
      this.renderRail();
      this.resetAndLoad();
    });
    return button;
  }

  renderMemberFilters() {
    if (!this.memberFilters) return;
    this.memberFilters.innerHTML = "";

    const allChip = this.createMemberChip(null, "All members", this.loadedCountForMember(null));
    this.memberFilters.appendChild(allChip);

    for (const member of this.members) {
      this.memberFilters.appendChild(
        this.createMemberChip(member.name, member.name, this.loadedCountForMember(member.name))
      );
    }
  }

  createMemberChip(memberName, label, count) {
    const chip = document.createElement("button");
    chip.className = "activity-feed-page__member-chip";
    chip.classList.toggle("activity-feed-page__member-chip--active", this.selectedMember === memberName);
    chip.innerHTML = `
      ${memberName ? `<player-icon player-name="${memberName}"></player-icon>` : ""}
      <span class="activity-feed-page__member-chip-label">${label}</span>
      <span class="activity-feed-page__member-chip-count">${count}</span>
    `;
    this.eventListener(chip, "click", () => {
      if (this.selectedMember === memberName) return;
      this.selectedMember = memberName;
      this.renderMemberFilters();
      this.resetAndLoad();
    });
    return chip;
  }

  createRow(event) {
    const row = document.createElement("activity-feed-event");
    row.reactionsEnabled = this.reactionsEnabled;
    row.event = event;
    return row;
  }

  // Loaded once per resetAndLoad (group settings don't change mid-session on this page) rather
  // than per-poll.
  async loadReactionSettings() {
    const settings = await api.getActivitySettings();
    this.reactionsEnabled = Boolean(settings.reactions_enabled);
  }

  // Refreshes reaction/comment-count data for whatever's currently rendered - a reaction or
  // comment never changes an event's `occurred_at`, so it would never surface through `poll()`'s
  // "only prepend strictly newer events" diff otherwise. Capped to the most recent rendered rows
  // (matching `get-activity-reactions`' own server-side batch cap) rather than every row ever
  // loaded, since only what's on screen (or just above it) is worth refreshing every 15s.
  async refreshReactions() {
    if (!this.reactionsEnabled) return;
    const rows = [...this.list.children].filter((row) => row.event).slice(0, 100);
    if (!rows.length) return;
    const ids = rows.map((row) => row.event.id);
    const summaries = await api.getActivityReactions(ids);
    for (const row of rows) {
      const summary = summaries[row.event.id];
      if (summary) row.applyReactionSummary(summary);
    }
  }

  // Folds a repeat kill/death against the same boss by the same member into its existing
  // aggregated row (see `KILL_MERGE_WINDOW_MS`) instead of creating a new one. Returns the row to
  // insert, or null when the event was merged into an already-rendered row instead.
  mergeOrCreateRow(event, { prepend }) {
    const displayType = activityDisplayType(event);
    // Kill and death groups are namespaced separately (rather than sharing `killGroupKey` raw) so
    // a kill and a death against the same NPC name - e.g. killed then later killed-by the same
    // boss - don't collide in `feedGroups`.
    const groupKey = (type, e) => `${type}|${killGroupKey(e)}`;
    // A death ends whatever kill streak was building for that member - the next kill of the same
    // boss (whichever direction in time it's encountered from, see `loadMore` vs `poll`) should
    // start a fresh aggregated row rather than folding into the one from before the death.
    if (displayType === "death") {
      for (const key of this.feedGroups.keys()) {
        if (key.startsWith(`kill|${event.member_name}|`)) this.feedGroups.delete(key);
      }
    }

    if (displayType === "kill" || displayType === "death") {
      const key = groupKey(displayType, event);
      const group = this.feedGroups.get(key);
      const eventTime = new Date(event.occurred_at).getTime();
      // A combo kill (Barrows/Moons of Peril) whose sub_kills differ from the row it would
      // otherwise fold into (e.g. one Moons attempt killing Eclipse+Blue+Blood, the next only
      // Blue) is a distinct kill, not a repeat of the same one - merging it would silently drop
      // its own breakdown (only the first event's sub_kills ever got kept) and misreport it as
      // "x2" of the earlier combo. Fall through to a fresh row instead.
      const sameSubKills = displayType !== "kill" || subKillsMatch(group?.event.payload, event.payload);
      // Doom of Mokhaiotl only: a level-1 and a level-4 kill are as distinct as two different
      // combo breakdowns above - see `delveLevelMatch`.
      const sameDelveLevel = displayType !== "kill" || delveLevelMatch(group?.event.payload, event.payload);
      if (
        group &&
        sameSubKills &&
        sameDelveLevel &&
        Math.abs(eventTime - new Date(group.event.occurred_at).getTime()) <= KILL_MERGE_WINDOW_MS
      ) {
        group.event.aggregateCount = (group.event.aggregateCount || 1) + 1;
        if (displayType === "kill") {
          group.event.payload = {
            ...group.event.payload,
            loot: [...(group.event.payload?.loot || []), ...(event.payload?.loot || [])],
          };
        }
        if (eventTime > new Date(group.event.occurred_at).getTime()) group.event.occurred_at = event.occurred_at;
        group.row.event = group.event;
        group.row.render();
        // Only a fresh, more recent event (poll) should bump the row back to the top - an older
        // event filled in from loadMore already sits above the batch being appended below it.
        if (prepend) this.list.insertBefore(group.row, this.list.firstChild);
        return null;
      }
    }

    const row = this.createRow(event);
    if (displayType === "kill" || displayType === "death") {
      this.feedGroups.set(groupKey(displayType, event), { event, row });
    }
    return row;
  }

  async resetAndLoad() {
    this.loaded = [];
    this.exhausted = false;
    this.autoLoadStreak = 0;
    this.autoLoadPaused = false;
    this.feedGroups = new Map();
    this.list.innerHTML = "";
    this.empty.classList.remove("activity-feed-page__empty--visible");
    this.sentinel.classList.remove("activity-feed-page__sentinel--paused");
    // The list is about to be wiped and rebuilt, so any selected rows are about to become stale
    // DOM references - drop out of selecting mode rather than carry them over.
    if (this.isAdmin) this.setSelecting(false);
    // Must resolve before loadMore's createRow calls so the first page's rows are built with the
    // right reactionsEnabled flag instead of defaulting to false and needing a retrofit.
    await this.loadReactionSettings();
    await Promise.all([this.loadMore(), this.loadCounts()]);
    await this.refreshReactions();
  }

  setSelecting(selecting) {
    this.selecting = selecting;
    if (!selecting) {
      this.selectedIds.clear();
      for (const row of this.list.children) {
        row.classList.remove("activity-feed-event--selected");
      }
    }
    this.selectToggle.textContent = selecting ? "Cancel select" : "Select";
    this.list.classList.toggle("activity-feed-page__list--selecting", selecting);
    this.modBar.classList.toggle("activity-feed-page__mod-bar--visible", selecting);
    this.updateModCount();
  }

  updateModCount() {
    this.modCount.textContent = this.selectedIds.size;
    this.modDeleteButton.disabled = this.selectedIds.size === 0;
  }

  handleRowClick(e) {
    if (!this.selecting) return;
    const row = e.target.closest("activity-feed-event");
    if (!row || !row.event) return;
    // Selecting mode repurposes the whole row as the tap target, including what would otherwise
    // be an outbound wiki link (see activity-feed-event.js's `subject` renderer) - never navigate
    // away from moderation because of a click meant to select.
    e.preventDefault();

    const id = row.event.id;
    if (this.selectedIds.has(id)) {
      this.selectedIds.delete(id);
      row.classList.remove("activity-feed-event--selected");
    } else {
      this.selectedIds.add(id);
      row.classList.add("activity-feed-event--selected");
    }
    this.updateModCount();
  }

  confirmDeleteSelected() {
    const ids = [...this.selectedIds];
    if (!ids.length) return;
    confirmDialogManager.confirm({
      headline: `Delete ${ids.length} event${ids.length === 1 ? "" : "s"}?`,
      body: "These events are removed from the activity feed for every member of this group. This cannot be undone.",
      yesCallback: () => this.deleteSelected(ids),
      noCallback: () => {},
    });
  }

  async deleteSelected(ids) {
    const response = await adminApi.deleteActivityEvents(this.adminGroupId, ids);
    if (!response.ok) return;

    const idSet = new Set(ids);
    this.loaded = this.loaded.filter((event) => !idSet.has(event.id));
    this.typeCountEvents = this.typeCountEvents.filter((event) => !idSet.has(event.id));
    this.memberCountEvents = this.memberCountEvents.filter((event) => !idSet.has(event.id));
    // A deleted event may be the one a later same-boss kill would otherwise merge into (see
    // `mergeOrCreateRow`) - drop it so that merge starts a fresh row instead of resurrecting the
    // deleted one's content.
    for (const [key, group] of this.feedGroups) {
      if (idSet.has(group.event.id)) this.feedGroups.delete(key);
    }
    for (const row of Array.from(this.list.children)) {
      if (row.event && idSet.has(row.event.id)) row.remove();
    }

    this.setSelecting(false);
    this.renderRail();
    this.renderMemberFilters();
    this.empty.classList.toggle("activity-feed-page__empty--visible", this.loaded.length === 0);
  }

  async loadCounts() {
    if (!api.groupName) return;
    const [typeCountEvents, memberCountEvents] = await Promise.all([
      api.getActivityEvents({ memberName: this.selectedMember, limit: COUNT_LIMIT }),
      api.getActivityEvents({ eventType: this.selectedType, limit: COUNT_LIMIT }),
    ]);
    if (this.disposed) return;
    this.typeCountEvents = typeCountEvents;
    this.memberCountEvents = memberCountEvents;
    this.renderRail();
    this.renderMemberFilters();
  }

  async loadMore({ manual = false } = {}) {
    if (this.loadingMore || this.exhausted) return;
    if (this.autoLoadPaused && !manual) return;
    // No group session to load against - e.g. it was invalidated mid-view (see disconnectedCallback).
    // Bail out before touching any state so a stray call can't leave the sentinel/button stuck.
    if (!api.groupName) return;
    if (manual) {
      this.autoLoadPaused = false;
      this.autoLoadStreak = 0;
      this.sentinel.classList.remove("activity-feed-page__sentinel--paused");
    } else {
      this.autoLoadStreak++;
    }
    this.loadingMore = true;
    this.sentinel.classList.add("activity-feed-page__sentinel--visible");
    this.sentinel.classList.add("activity-feed-page__sentinel--loading");

    const before = this.loaded.length ? this.loaded[this.loaded.length - 1].occurred_at : undefined;
    const page = await api.getActivityEvents({
      memberName: this.selectedMember,
      eventType: this.selectedType,
      before,
      limit: PAGE_LIMIT,
    });

    this.loadingMore = false;
    this.sentinel.classList.remove("activity-feed-page__sentinel--loading");
    if (this.disposed) return;

    for (const event of page) {
      this.loaded.push(event);
      const row = this.mergeOrCreateRow(event, { prepend: false });
      if (row) this.list.appendChild(row);
    }
    this.exhausted = page.length < PAGE_LIMIT;
    this.autoLoadPaused = !this.exhausted && this.autoLoadStreak >= AUTO_LOAD_BURST_LIMIT;
    this.sentinel.classList.toggle("activity-feed-page__sentinel--visible", !this.exhausted);
    this.sentinel.classList.toggle("activity-feed-page__sentinel--paused", this.autoLoadPaused);
    this.empty.classList.toggle("activity-feed-page__empty--visible", this.loaded.length === 0);
    this.renderRail();
    this.renderMemberFilters();
    await this.refreshReactions();
  }

  // Fetches the newest page and prepends whatever is newer than what's already loaded, leaving
  // already-loaded older pages untouched. Preserves the reader's scroll position by measuring how
  // much content height the prepend adds and offsetting scrollTop by the same amount, so a poll
  // firing while the user is scrolled down never yanks the viewport back to the top.
  async poll() {
    if (this.loadingMore || !this.loaded.length || !api.groupName) return;

    const newestOccurredAt = new Date(this.loaded[0].occurred_at).getTime();
    const page = await api.getActivityEvents({
      memberName: this.selectedMember,
      eventType: this.selectedType,
      limit: PAGE_LIMIT,
    });
    if (this.disposed) return;
    // Runs every tick regardless of whether any event is actually newer - a reaction/comment on
    // an already-loaded row never changes its `occurred_at`, so it would never be caught by the
    // "newer than last seen" diff below otherwise (see `refreshReactions`'s own doc comment).
    await this.refreshReactions();
    const incoming = page.filter((event) => new Date(event.occurred_at).getTime() > newestOccurredAt);
    if (!incoming.length) return;

    const scrollTopBefore = this.scrollContainer.scrollTop;
    const heightBefore = this.list.scrollHeight;

    // Oldest-of-batch first so each insertion lands above the previous one, keeping merge
    // repositioning (see `mergeOrCreateRow`) in the right chronological order at the top.
    for (const event of [...incoming].reverse()) {
      this.loaded.unshift(event);
      const row = this.mergeOrCreateRow(event, { prepend: true });
      if (row) {
        row.classList.add("activity-feed-event--enter");
        this.list.insertBefore(row, this.list.firstChild);
      }
    }

    if (scrollTopBefore > 0) {
      const delta = this.list.scrollHeight - heightBefore;
      this.scrollContainer.scrollTop = scrollTopBefore + delta;
    }

    await this.loadCounts();
  }
}

customElements.define("activity-feed-page", ActivityFeedPage);
