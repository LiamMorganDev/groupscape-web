import { BaseElement } from "../base-element/base-element";
import { api } from "../data/api";
import { utility } from "../utility";
import { Item } from "../data/item";
import { timeBounds, formatDuration, formatTimeRange } from "../data/time-range";

// Server clamps this to 100 (see get_loot_log's `query.limit.clamp(1, 100)`) - use the max so a
// typical group's history resolves in as few round trips as possible, since every trip risks
// landing mid-session and needing the GROUP_CLOSE_FETCH_CAP chase below.
const PAGE_LIMIT = 100;
const REFRESH_INTERVAL_MS = 15000;
const SEARCH_DEBOUNCE_MS = 1000;
// Same reasoning as activity-feed-page.js's AUTO_LOAD_BURST_LIMIT - a search can scan page after
// page of raw history without a single match (server-side scan cap notwithstanding), so bound how
// many pages the sentinel auto-fetches before requiring a manual click, rather than trying to
// detect "made no progress" (a near-miss search can also add a trickle of real matches per page).
// Kept fairly high, unlike activity-feed's, since a real page here (see PAGE_LIMIT) already does
// much more work per request - a burst of these covers a lot more history before pausing.
const AUTO_LOAD_BURST_LIMIT = 10;
// Unconditional backstop on the sentinel auto-load loop, independent of autoLoadPaused/exhausted -
// guarantees the loop can never run away even if that state is ever wrong.
const AUTO_LOAD_HARD_CAP = 20;
// Consecutive same-member/source/type events with a gap under this are one farming session -
// see the locked Loot Log design doc's "45-minute rolling window" rule.
const SESSION_MERGE_WINDOW_MS = 45 * 60 * 1000;
// A session's group card must never grow after it's been shown to the user - so the oldest
// (trailing) card produced by a loadMore() page can't be considered "loaded" until we know its
// 45-minute window is actually closed, which requires peeking at the next page too. Bounds how
// many extra pages a single loadMore() call may fetch chasing that closure - same reasoning as
// AUTO_LOAD_BURST_LIMIT: an uninterrupted same-boss farming session can span many pages, so this
// has to give up and leave the trailing card open (it'll keep resolving on subsequent loadMore()
// calls) rather than risk the runaway-fetch tab freeze this file has been bitten by before.
const GROUP_CLOSE_FETCH_CAP = 5;
const MAX_RESOLVED_ITEM_IDS = 200;
const PLACEHOLDER_EXAMPLES = [
  "zulrah",
  ">1m",
  "whip",
  "732",
  "master clue",
  "<100k",
  "vorkath && >1m",
  "vorkath && >10kc",
  "unique",
  "rare",
  "<1/5000",
  "chest && >1m",
  "today",
  "tormented demons and today",
];
const PLACEHOLDER_INTERVAL_MS = 2500;
const PLACEHOLDER_FADE_MS = 400;
const FILTER_CATEGORIES = [
  { key: "boss", label: "Bosses" },
  { key: "other", label: "Other kills" },
  { key: "chest", label: "Chests" },
  { key: "clue", label: "Clues" },
];
const ALL_CATEGORY_KEYS = FILTER_CATEGORIES.map((c) => c.key);
// Deselecting the last active pill snaps back to all-selected (see toggleCategory) rather than
// letting the filter go to "show nothing", so this key only ever needs to store a real subset.
const CATEGORIES_STORAGE_KEY = "groupscape:loot-log-categories";

// Restores the last-selected filter pills across visits. Falls back to all-selected on a first
// visit, a corrupted value, or a stored subset that's since gone empty/invalid - the filter
// pills must always resolve to a real, non-empty subset of FILTER_CATEGORIES.
function loadPersistedCategories() {
  let stored;
  try {
    stored = JSON.parse(window.localStorage.getItem(CATEGORIES_STORAGE_KEY));
  } catch {
    return [...ALL_CATEGORY_KEYS];
  }
  if (!Array.isArray(stored) || !stored.length) return [...ALL_CATEGORY_KEYS];
  const valid = stored.filter((key) => ALL_CATEGORY_KEYS.includes(key));
  return valid.length ? valid : [...ALL_CATEGORY_KEYS];
}

// Kill count ("kc") is a farming-session concept - the number of events merged into one
// loot-log-group card (see that component's `countLabel`) - that only exists client-side after
// session grouping, unlike the quantity/level clauses (and combined value clauses like
// "vorkath && >1m") the server filters raw events on (see loot_log_search.rs). A *bare* value
// clause (the whole search is just ">100k", or one side of an "&&") is also deferred to the
// client for the same reason - see `parseValueClauses` below and the server's matching
// `bare_value_group` carve-out in authed.rs. So the raw search text is re-parsed here,
// independently of what the server did with it, purely to decide whether to show/hide each
// session card. Mirrors loot_log_search.rs's `split_search_groups`/`parse_numeric_clause` grammar
// - keep both in sync if either changes.
const KILL_COUNT_WORDS = ["kc", "kills", "kill"];

function parseNumericClauseJs(token) {
  const t = token.trim();
  if (!t) return null;
  let op = "eq";
  let rest = t;
  if (rest.startsWith(">=")) {
    op = "gte";
    rest = rest.slice(2);
  } else if (rest.startsWith("<=")) {
    op = "lte";
    rest = rest.slice(2);
  } else if (rest.startsWith(">")) {
    op = "gt";
    rest = rest.slice(1);
  } else if (rest.startsWith("<")) {
    op = "lt";
    rest = rest.slice(1);
  } else if (rest.startsWith("=")) {
    rest = rest.slice(1);
  }
  rest = rest.trim();
  if (!rest) return null;

  const lastChar = rest[rest.length - 1].toLowerCase();
  let numberPart = rest;
  let multiplier = 1;
  if (lastChar === "k") {
    numberPart = rest.slice(0, -1);
    multiplier = 1_000;
  } else if (lastChar === "m") {
    numberPart = rest.slice(0, -1);
    multiplier = 1_000_000;
  } else if (lastChar === "b") {
    numberPart = rest.slice(0, -1);
    multiplier = 1_000_000_000;
  }
  const cleaned = numberPart.replace(/,/g, "");
  if (!cleaned) return null;
  const base = Number(cleaned);
  if (!Number.isFinite(base)) return null;
  return { op, value: base * multiplier };
}

function numericClauseMatchesJs(clause, candidate) {
  switch (clause.op) {
    case "gt":
      return candidate > clause.value;
    case "gte":
      return candidate >= clause.value;
    case "lt":
      return candidate < clause.value;
    case "lte":
      return candidate <= clause.value;
    default:
      return Math.abs(candidate - clause.value) < 0.5;
  }
}

// "&&" and the standalone word "and" (case-insensitive) split into AND-required groups; plain
// whitespace does not, since NPC/item names routinely contain spaces.
function splitSearchGroups(search) {
  const spaced = search.replace(/&&/g, " && ");
  const groups = [[]];
  for (const word of spaced.split(/\s+/).filter(Boolean)) {
    if (word === "&&" || word.toLowerCase() === "and") {
      if (groups[groups.length - 1].length) groups.push([]);
      continue;
    }
    groups[groups.length - 1].push(word);
  }
  return groups.filter((group) => group.length);
}

// Finds a kill-count clause within one AND-group of tokens, either attached to the number
// ("10kc", like the k/m/b multiplier suffixes) or as its own following word ("10 kills"), and
// returns the clause plus the group's remaining tokens with it removed.
function extractKillCountClause(tokens) {
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    const lower = token.toLowerCase();
    for (const word of KILL_COUNT_WORDS) {
      if (lower.length > word.length && lower.endsWith(word)) {
        const clause = parseNumericClauseJs(token.slice(0, token.length - word.length));
        if (clause) {
          return { clause, rest: [...tokens.slice(0, i), ...tokens.slice(i + 1)] };
        }
      }
    }
  }
  for (let i = 0; i < tokens.length - 1; i++) {
    const clause = parseNumericClauseJs(tokens[i]);
    if (clause && KILL_COUNT_WORDS.includes(tokens[i + 1].toLowerCase())) {
      return { clause, rest: [...tokens.slice(0, i), ...tokens.slice(i + 2)] };
    }
  }
  return { clause: null, rest: tokens };
}

// A group counts as a kill-count clause only when the whole group was that clause (e.g.
// "vorkath && >10kc") - a group like "vorkath 10kc" (no separator) still needs "vorkath" checked
// server-side too, so it's left alone here rather than being misread as an unconditional kc gate.
function parseKillCountClauses(search) {
  const clauses = [];
  for (const group of splitSearchGroups(search || "")) {
    const { clause, rest } = extractKillCountClause(group);
    if (clause && rest.length === 0) clauses.push(clause);
  }
  return clauses;
}

// Mirrors the server's "bare value group" carve-out (see build_matching_loot_log_event's
// `bare_value_group` in authed.rs) - a group that's nothing but one value clause (the whole
// search is just ">100k", or one side of "vorkath && >100k") reads as "worth over X" against the
// number actually shown on a session card (see loot-log-group.js's `totalValue`), not a lone raw
// event's own total, so the server lets those events through unfiltered and this re-applies the
// clause here, once session merging has actually happened (see `matchesValue`). Keep in sync with
// the server's carve-out condition.
function parseValueClauses(search) {
  const clauses = [];
  for (const group of splitSearchGroups(search || "")) {
    const { clause: killCountClause, rest } = extractKillCountClause(group);
    if (killCountClause) continue;
    if (rest.length === 1) {
      const clause = parseNumericClauseJs(rest[0]);
      if (clause) clauses.push(clause);
    }
  }
  return clauses;
}

export class LootLogPage extends BaseElement {
  constructor() {
    super();
    this.disposed = false;
    this.loaded = [];
    this.nextBefore = undefined;
    this.exhausted = false;
    this.loadingMore = false;
    // The loadGeneration this.loadingMore's in-flight fetch belongs to, or null when idle -
    // lets a new generation's loadMore() start immediately even while an old (now-stale)
    // generation's fetch is still awaiting its response, instead of being blocked by it.
    this.loadingGeneration = null;
    this.autoLoadStreak = 0;
    this.autoLoadPaused = false;
    // Total events added since the streak last reset (manual click or resetAndLoad) - lets
    // loadMore() tell "still making progress, just needs another click" apart from "repeatedly
    // scanned and found nothing", so the sentinel can hide itself in the latter case instead of
    // dangling a "Load more" that's unlikely to ever surface anything (see `autoLoadGaveUp`).
    this.autoLoadStreakEventsAdded = 0;
    this.autoLoadGaveUp = false;
    this.searchText = "";
    this.killCountClauses = [];
    this.valueClauses = [];
    this.itemIds = [];
    this.categories = loadPersistedCategories();
    this.summary = { total_value: 0, event_count: 0 };
    // Bumped by resetAndLoad() so an in-flight loadMore()/loadSummary() from a search/category
    // that's since changed can detect it's stale and discard its results instead of appending
    // them into the freshly-cleared list - without this, a slow fetch started under the old
    // filter can land after the reset and silently reintroduce unfiltered cards.
    this.loadGeneration = 0;
    // key (member+source+type+clueTier, see `entryKey`) -> { key, events, element } for the
    // farming-session entry currently shown for that key, so an event within the 45-minute
    // window extends it instead of adding a new entry.
    this.entryGroups = new Map();
  }

  html() {
    return `{{loot-log-page.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    // app-route reuses the same page instance across navigations (only constructs it once - see
    // app-route.js's `enable()`), so disconnectedCallback's `disposed = true` would otherwise stick
    // forever after the first time this page is navigated away from, silently no-op'ing every
    // loadMore/loadSummary/poll call on every future visit.
    this.disposed = false;
    this.render();

    this.summaryContainer = this.querySelector(".loot-log-page__summary");
    this.filtersContainer = this.querySelector(".loot-log-page__filters");
    this.renderFilters();
    this.eventListener(this.filtersContainer, "click", (event) => {
      const chip = event.target.closest(".loot-log-page__filter-chip");
      if (chip) this.toggleCategory(chip.dataset.category);
    });
    this.searchInput = this.querySelector(".loot-log-page__search");
    this.placeholderLabel = this.querySelector(".loot-log-page__search-placeholder");
    this.list = this.querySelector(".loot-log-page__list");
    this.sentinel = this.querySelector(".loot-log-page__sentinel");
    this.empty = this.querySelector(".loot-log-page__empty");
    this.loadMoreButton = this.querySelector(".loot-log-page__load-more");
    this.scrollContainer = this.closest(".authed-section__main-content") || document.documentElement;

    this.eventListener(this.loadMoreButton, "click", () => this.loadMore({ manual: true }));
    this.eventListener(this.searchInput, "input", () => {
      this.stopPlaceholderCycle();
      window.clearTimeout(this.searchDebounceTimer);
      this.searchDebounceTimer = window.setTimeout(() => this.applySearch(), SEARCH_DEBOUNCE_MS);
    });
    this.eventListener(this.searchInput, "focus", () => this.stopPlaceholderCycle());
    this.eventListener(this.searchInput, "blur", () => {
      if (!this.searchInput.value) this.startPlaceholderCycle();
    });

    if (!this.searchInput.value) this.startPlaceholderCycle();

    this.intersectionObserver = new IntersectionObserver(this.handleSentinelIntersect.bind(this), {
      root: this.scrollContainer,
      rootMargin: "120px",
    });
    this.intersectionObserver.observe(this.sentinel);

    // On a hard refresh landing directly on this page, this component's connectedCallback can run
    // before app-initializer's api.enable() finishes setting the group credentials - loading then
    // instead of waiting for it fired requests as `/api/group/undefined/...`, which 401 and get
    // misread as "no more history", permanently marking the page exhausted with nothing loaded.
    // Wait for the first successful group-data fetch (the same signal app-initializer itself waits
    // on) before loading if the group isn't ready yet.
    if (api.groupName) {
      this.resetAndLoad();
    } else {
      this.subscribeOnce("get-group-data", () => this.resetAndLoad());
    }
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
    this.stopPlaceholderCycle();
    window.clearTimeout(this.searchDebounceTimer);
  }

  // IntersectionObserver only calls back on a threshold crossing, not continuously while
  // intersecting - a farming session collapses into one <loot-log-group> card, so loading another
  // page can leave the sentinel exactly where it was and the observer never fires again.
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

  startPlaceholderCycle() {
    if (!this.placeholderLabel || this.placeholderInterval) return;
    this.placeholderIndex = 0;
    this.placeholderLabel.textContent = PLACEHOLDER_EXAMPLES[0];
    this.placeholderLabel.classList.remove("loot-log-page__search-placeholder--hidden");
    this.placeholderInterval = window.setInterval(() => {
      this.placeholderLabel.classList.add("loot-log-page__search-placeholder--hidden");
      window.setTimeout(() => {
        this.placeholderIndex = (this.placeholderIndex + 1) % PLACEHOLDER_EXAMPLES.length;
        this.placeholderLabel.textContent = PLACEHOLDER_EXAMPLES[this.placeholderIndex];
        this.placeholderLabel.classList.remove("loot-log-page__search-placeholder--hidden");
      }, PLACEHOLDER_FADE_MS);
    }, PLACEHOLDER_INTERVAL_MS);
  }

  stopPlaceholderCycle() {
    if (this.placeholderInterval) {
      window.clearInterval(this.placeholderInterval);
      this.placeholderInterval = null;
    }
    if (this.placeholderLabel) this.placeholderLabel.classList.add("loot-log-page__search-placeholder--hidden");
  }

  // Heuristic client-side item-name resolution: the server has no full item-name table (only
  // curated drop_rates entries), so it can't search on item name itself - the client resolves
  // matching item ids from the full catalog (Item.itemDetails) and sends them as `item_ids` for
  // the server to test against each event's loot. Capped so a broad query can't blow up the URL.
  resolveItemIds(query) {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length || !Item.itemDetails) return [];
    const ids = [];
    for (const [id, details] of Object.entries(Item.itemDetails)) {
      const name = (details.name || "").toLowerCase();
      if (words.some((word) => name.includes(word))) {
        ids.push(Number(id));
        if (ids.length >= MAX_RESOLVED_ITEM_IDS) break;
      }
    }
    return ids;
  }

  renderFilters() {
    this.filtersContainer.innerHTML = FILTER_CATEGORIES.map(({ key, label }) => {
      const active = this.categories.includes(key);
      return `
        <button
          type="button"
          class="loot-log-page__filter-chip${active ? " loot-log-page__filter-chip--active" : ""}"
          data-category="${key}"
        >
          <span class="loot-log-page__filter-chip-dot"></span>${label}
        </button>
      `;
    }).join("");
  }

  // Deselecting the last active pill snaps back to all-selected rather than leaving the filter
  // in a "show nothing" state - see loadPersistedCategories's same invariant for storage.
  toggleCategory(key) {
    const active = this.categories.includes(key);
    if (active && this.categories.length === 1) {
      this.categories = [...ALL_CATEGORY_KEYS];
    } else if (active) {
      this.categories = this.categories.filter((k) => k !== key);
    } else {
      this.categories = [...this.categories, key];
    }
    window.localStorage.setItem(CATEGORIES_STORAGE_KEY, JSON.stringify(this.categories));
    this.renderFilters();
    this.resetAndLoad();
  }

  applySearch() {
    const raw = this.searchInput.value.trim();
    this.searchText = raw;
    this.itemIds = this.resolveItemIds(raw);
    this.killCountClauses = parseKillCountClauses(raw);
    this.valueClauses = parseValueClauses(raw);
    this.resetAndLoad();
  }

  // Whether a session card with this many merged events satisfies every kill-count clause in the
  // current search (see `parseKillCountClauses`) - AND across clauses, same as every other group.
  matchesKillCount(count) {
    return this.killCountClauses.every((clause) => numericClauseMatchesJs(clause, count));
  }

  // Matches loot-log-group.js's `mergedItems`/`totalValue`: an item the client's catalog doesn't
  // recognize (no name/image/value on file) is dropped from the card entirely, not just dimmed -
  // so it must also be dropped here, or a session could clear a value clause using gp from an
  // item the user can't actually see on the card that's supposedly matching.
  eventTotalValue(event) {
    return event.items.reduce((sum, item) => sum + (Item.exists(item.item_id) ? item.total_value : 0), 0);
  }

  // Whether a session card built from these merged events satisfies every bare value clause in
  // the current search (see `parseValueClauses`) - AND across clauses, same as every other group,
  // with each clause independently satisfied by either the session's merged total or any single
  // underlying event's own total (a session made of one huge kill and many tiny ones should still
  // surface for e.g. ">1m" even though its own total is smaller than the search intends to gate on).
  matchesValue(events) {
    if (!this.valueClauses.length) return true;
    const sessionTotal = events.reduce((sum, event) => sum + this.eventTotalValue(event), 0);
    return this.valueClauses.every(
      (clause) =>
        numericClauseMatchesJs(clause, sessionTotal) ||
        events.some((event) => numericClauseMatchesJs(clause, this.eventTotalValue(event)))
    );
  }

  // Combines both client-side-only filters a session card must satisfy - see `matchesKillCount`
  // and `matchesValue`.
  matchesClauses(events) {
    return this.matchesKillCount(events.length) && this.matchesValue(events);
  }

  // Doom of Mokhaiotl's delve_level is folded in alongside clue_tier so two runs at different
  // levels never merge into one session card - a level-1 and a level-4 kill close together are
  // as distinct as two different clue tiers, not a repeat of the same farming session.
  entryKey(event) {
    return `${event.member_name}|${event.source_name}|${event.source_type}|${event.clue_tier || ""}|${
      event.delve_level ?? ""
    }`;
  }

  buildGroupData(entry) {
    const newest = entry.events[0];
    return {
      memberName: newest.member_name,
      sourceName: newest.source_name,
      sourceType: newest.source_type,
      clueTier: newest.clue_tier,
      delveLevel: newest.delve_level,
      events: entry.events,
    };
  }

  // Appended when paging older history in (see `loadMore`) - events arrive newest-first within a
  // page, so each new event is older than whatever's already tracked for its key. A `frozen`
  // entry (see `loadMore`'s GROUP_CLOSE_FETCH_CAP handling) has already been shown to the user as
  // a finished card and must never grow again, even if a later event would otherwise fall inside
  // its window - falls through to starting a fresh entry for that key instead.
  appendEvent(event) {
    const key = this.entryKey(event);
    const entry = this.entryGroups.get(key);
    const eventTime = new Date(event.occurred_at).getTime();
    if (entry && !entry.frozen) {
      const oldestTime = new Date(entry.events[entry.events.length - 1].occurred_at).getTime();
      if (oldestTime - eventTime <= SESSION_MERGE_WINDOW_MS) {
        entry.events.push(event);
        entry.element.group = this.buildGroupData(entry);
        entry.element.update();
        entry.element.hidden = !this.matchesClauses(entry.events);
        return;
      }
    }
    const newEntry = { key, events: [event], frozen: false };
    newEntry.element = document.createElement("loot-log-group");
    newEntry.element.group = this.buildGroupData(newEntry);
    newEntry.element.hidden = !this.matchesClauses(newEntry.events);
    this.list.appendChild(newEntry.element);
    this.entryGroups.set(key, newEntry);
  }

  // Prepended when polling for fresher events (see `poll`) - events arrive oldest-of-batch first
  // (caller reverses the incoming batch), so each new event is newer than whatever's tracked.
  // Same `frozen` guard as `appendEvent`, though in practice a poll's fresh events landing inside
  // a frozen (old, already-closed-out) entry's window would be a rare coincidence.
  prependEvent(event) {
    const key = this.entryKey(event);
    const entry = this.entryGroups.get(key);
    const eventTime = new Date(event.occurred_at).getTime();
    if (entry && !entry.frozen) {
      const newestTime = new Date(entry.events[0].occurred_at).getTime();
      if (eventTime - newestTime <= SESSION_MERGE_WINDOW_MS) {
        entry.events.unshift(event);
        entry.element.group = this.buildGroupData(entry);
        entry.element.update();
        entry.element.hidden = !this.matchesClauses(entry.events);
        this.list.insertBefore(entry.element, this.list.firstChild);
        return;
      }
    }
    const newEntry = { key, events: [event], frozen: false };
    newEntry.element = document.createElement("loot-log-group");
    newEntry.element.group = this.buildGroupData(newEntry);
    newEntry.element.hidden = !this.matchesClauses(newEntry.events);
    this.list.insertBefore(newEntry.element, this.list.firstChild);
    this.entryGroups.set(key, newEntry);
  }

  async resetAndLoad() {
    // Invalidates any loadMore()/loadSummary() fetch already in flight under the old search/
    // category state - see loadGeneration's doc comment.
    this.loadGeneration++;
    this.loaded = [];
    this.nextBefore = undefined;
    this.exhausted = false;
    this.autoLoadStreak = 0;
    this.autoLoadPaused = false;
    this.autoLoadStreakEventsAdded = 0;
    this.autoLoadGaveUp = false;
    this.entryGroups = new Map();
    this.list.innerHTML = "";
    this.empty.classList.remove("loot-log-page__empty--visible");
    this.sentinel.classList.remove("loot-log-page__sentinel--paused");
    await Promise.all([this.loadMore(), this.loadSummary()]);
  }

  async loadSummary() {
    if (!api.groupName) return;
    const generation = this.loadGeneration;
    const summary = await api.getLootLogSummary({
      search: this.searchText,
      itemIds: this.itemIds,
      categories: this.categories,
    });
    if (this.disposed || generation !== this.loadGeneration) return;
    this.summary = summary;
    this.renderSummary();
  }

  async loadMore({ manual = false } = {}) {
    if (this.exhausted) return;
    if (this.autoLoadPaused && !manual) return;
    // No group session to load against - e.g. it was invalidated mid-view (see disconnectedCallback).
    // Bail out before touching any state so a stray call can't leave the sentinel/button stuck.
    if (!api.groupName) return;
    const generation = this.loadGeneration;
    // Reentrancy guard, scoped to this generation - a stale generation's own fetch loop is left
    // to run its course and self-detect staleness below rather than blocking a fresh reload.
    if (this.loadingGeneration === generation) return;
    if (manual) {
      this.autoLoadPaused = false;
      this.autoLoadStreak = 0;
      this.autoLoadStreakEventsAdded = 0;
      this.autoLoadGaveUp = false;
      this.sentinel.classList.remove("loot-log-page__sentinel--paused");
    } else {
      this.autoLoadStreak++;
    }
    this.loadingGeneration = generation;
    this.loadingMore = true;
    this.sentinel.classList.add("loot-log-page__sentinel--visible");
    this.sentinel.classList.add("loot-log-page__sentinel--loading");

    // Keep fetching until the trailing group is provably closed (see `GROUP_CLOSE_FETCH_CAP`):
    // a page's last event always leaves its group's fate unknown - only the next page's leading
    // event, once fetched, can tell us whether that group grows further. Events only get older
    // from here, so as soon as one page's first event fails to extend the current trailing group,
    // that group can never grow again and it's provably closed (no freeze needed - the window
    // check alone blocks any future merge into it). Each new page's own tail then becomes the
    // next trailing group to verify, chained until it's closed, exhausted, or the fetch budget
    // runs out.
    let trailingKey = null;
    let addedThisCall = 0;
    let fetches = 0;
    for (; fetches < GROUP_CLOSE_FETCH_CAP; fetches++) {
      const page = await api.getLootLog({
        before: this.nextBefore,
        limit: PAGE_LIMIT,
        search: this.searchText,
        itemIds: this.itemIds,
        categories: this.categories,
      });
      if (this.disposed || generation !== this.loadGeneration) {
        if (this.loadingGeneration === generation) {
          this.loadingMore = false;
          this.loadingGeneration = null;
        }
        return;
      }

      if (trailingKey && page.events.length) {
        const first = page.events[0];
        const entry = this.entryGroups.get(trailingKey);
        const extendsTrailing =
          entry &&
          !entry.frozen &&
          this.entryKey(first) === trailingKey &&
          new Date(entry.events[entry.events.length - 1].occurred_at).getTime() -
            new Date(first.occurred_at).getTime() <=
            SESSION_MERGE_WINDOW_MS;
        if (!extendsTrailing) trailingKey = null;
      }

      for (const event of page.events) {
        this.loaded.push(event);
        this.appendEvent(event);
      }
      addedThisCall += page.events.length;
      this.nextBefore = page.next_before || undefined;
      this.exhausted = !!page.scan_exhausted;
      trailingKey = this.exhausted || !page.events.length ? null : this.entryKey(page.events[page.events.length - 1]);

      if (!trailingKey) break;
    }

    // Fetch budget ran out chasing a group's closure without resolving it - freeze that card so
    // it can't keep silently growing on a future loadMore() call (see `appendEvent`'s `frozen`
    // guard). It stays as whatever it loaded here; a real session that long may render as more
    // than one card.
    if (trailingKey) {
      const openEntry = this.entryGroups.get(trailingKey);
      if (openEntry) openEntry.frozen = true;
    }

    if (this.loadingGeneration === generation) {
      this.loadingMore = false;
      this.loadingGeneration = null;
    }
    this.sentinel.classList.remove("loot-log-page__sentinel--loading");
    if (this.disposed || generation !== this.loadGeneration) return;

    this.autoLoadStreakEventsAdded += addedThisCall;
    this.autoLoadPaused = !this.exhausted && this.autoLoadStreak >= AUTO_LOAD_BURST_LIMIT;
    // The server's raw-row scan is capped per request (see get_loot_log's LOOT_LOG_SCAN_CAP), so
    // a long run of loot-less kills near the end of a group's history can repeatedly report "not
    // exhausted" without ever actually surfacing anything new. Once a full auto-load burst has
    // come back completely empty, treat it the same as exhausted for display purposes - a "Load
    // more" that's already proven fruitless a few times in a row isn't worth dangling in front of
    // the user, even though it's not a mathematically certain end of history.
    this.autoLoadGaveUp = this.autoLoadPaused && this.autoLoadStreakEventsAdded === 0;
    this.sentinel.classList.toggle("loot-log-page__sentinel--visible", !this.exhausted && !this.autoLoadGaveUp);
    this.sentinel.classList.toggle("loot-log-page__sentinel--paused", this.autoLoadPaused && !this.autoLoadGaveUp);
    this.empty.classList.toggle("loot-log-page__empty--visible", this.loaded.length === 0 && this.exhausted);
    this.renderSummary();
  }

  // Mirrors activity-feed-page.js's `poll` - fetches the newest page and prepends whatever is
  // newer than what's already loaded, preserving scroll position the same way.
  async poll() {
    if (this.loadingMore || !this.loaded.length || !api.groupName) return;

    const generation = this.loadGeneration;
    const newestOccurredAt = new Date(this.loaded[0].occurred_at).getTime();
    const page = await api.getLootLog({
      search: this.searchText,
      itemIds: this.itemIds,
      categories: this.categories,
      limit: PAGE_LIMIT,
    });
    if (this.disposed || generation !== this.loadGeneration) return;
    const incoming = page.events.filter((event) => new Date(event.occurred_at).getTime() > newestOccurredAt);
    if (!incoming.length) return;

    const scrollTopBefore = this.scrollContainer.scrollTop;
    const heightBefore = this.list.scrollHeight;

    for (const event of [...incoming].reverse()) {
      this.loaded.unshift(event);
      this.prependEvent(event);
    }

    if (scrollTopBefore > 0) {
      const delta = this.list.scrollHeight - heightBefore;
      this.scrollContainer.scrollTop = scrollTopBefore + delta;
    }

    await this.loadSummary();
  }

  renderSummary() {
    if (!this.summaryContainer) return;
    if (this.loaded.length === 0 && this.summary.event_count === 0) {
      this.summaryContainer.innerHTML = "";
      return;
    }
    this.summaryContainer.innerHTML = `
      <div class="loot-log-page__summary-stats">
        <div class="loot-log-page__summary-card">
          <div class="loot-log-page__summary-n">${this.summary.total_value.toLocaleString()}</div>
          <div class="loot-log-page__summary-l">Total Value</div>
        </div>
        <div class="loot-log-page__summary-card">
          <div class="loot-log-page__summary-n">${this.summary.event_count.toLocaleString()}</div>
          <div class="loot-log-page__summary-l">Events</div>
        </div>
        <div class="loot-log-page__summary-card loot-log-page__summary-card--session">
          <div class="loot-log-page__summary-n">${this.sessionDuration()}</div>
          <div class="loot-log-page__summary-l">${this.sessionRange()}</div>
        </div>
      </div>
    `;
  }

  // First loaded event -> last loaded event, not a detected play-session boundary - just the
  // span of what's currently paged in (loading more history extends it).
  sessionDuration() {
    if (!this.loaded.length) return "0m";
    const { first, last } = timeBounds(this.loaded);
    return formatDuration(first, last);
  }

  sessionRange() {
    if (!this.loaded.length) return "";
    const { first, last } = timeBounds(this.loaded);
    return formatTimeRange(first, last);
  }
}

customElements.define("loot-log-page", LootLogPage);
