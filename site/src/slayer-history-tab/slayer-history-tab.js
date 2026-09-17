import { BaseElement } from "../base-element/base-element";
import { api } from "../data/api";
import { slayerData } from "../data/slayer";
import { formatRelativeTimeLong } from "../data/relative-time";

const OPEN_STATUSES = new Set(["not_started", "in_progress"]);

const STATUS_OPTIONS = [
  { value: "", label: "All statuses" },
  { value: "in_progress", label: "In progress" },
  { value: "completed", label: "Completed" },
  { value: "cancelled", label: "Cancelled" },
  { value: "blocked", label: "Blocked" },
  { value: "reset", label: "Reset" },
  { value: "unknown", label: "Unknown" },
  { value: "superseded", label: "Superseded" },
];

const STATUS_META = {
  not_started: { label: "Not started", cls: "ns" },
  in_progress: { label: "In progress", cls: "ip" },
  completed: { label: "Completed", cls: "cp" },
  cancelled: { label: "Cancelled", cls: "cx" },
  blocked: { label: "Blocked", cls: "bl" },
  // A free Turael/Aya/Spria skip - no points spent, but it still resets the normal-bucket streak.
  // See GroupScapeTrackerPlugin#closeSlayerTask's SLAYER_RESET_MASTERS check (checks whichever
  // master granted the *new* task, not the closing one, so skipping any master's task via
  // Turael/Aya/Spria lands here rather than under "unknown").
  reset: { label: "Reset", cls: "rs" },
  // A points delta the plugin didn't recognize as any of the above (e.g. a game update changing
  // cancel/block prices, or a Mortimer task closing with neither a completed nor -30 delta) - see
  // GroupScapeTrackerPlugin#closeSlayerTask's javadoc.
  unknown: { label: "Unknown", cls: "sp" },
  // Server-side cleanup, not a real close reason - see db::upsert_slayer_task_history_event's
  // doc comment. Should now only show up for a genuinely unrecoverable dangling row (e.g. a
  // plugin/client restart mid-task with no surviving snapshot) - a lost close event for a skip
  // via Turael/Aya/Spria is recognized as "reset" and a fully-killed task as "completed" instead.
  superseded: { label: "Superseded", cls: "sp" },
};

// Grouped by slayer master *role*, not individual NPC - Aya/Spria are Turael reskins (post-
// "While Guthix Sleeps" and pre-quest respectively), Achtryn replaces Mazchna the same way, and
// Steve/Kuradal are temporary/quest-unlocked stand-ins for Nieve/Duradel. All share their base
// master's task list and block price (see GroupScapeTrackerPlugin's SLAYER_BLOCK_PRICE table),
// so a group's task history reads as one continuous master rather than splintering across
// whichever NPC happened to be at the desk that day. Ordered by unlock progression (Slayer
// Master wiki page's combat/Slayer level requirements), Krystilia/Mortimer last since they sit
// outside the normal ladder (wilderness-only and Miscellania-diary-gated respectively). Names
// are display-cased as the plugin captures them from dialogue (see
// GroupScapeTrackerPlugin#captureSlayerTaskMasterDialogue) - matched server-side by exact string
// equality against this list (see db::list_slayer_task_history_page), not case-insensitively.
const MASTER_GROUPS = [
  { label: "Turael", names: ["Turael", "Aya", "Spria"] },
  { label: "Mazchna", names: ["Mazchna", "Achtryn"] },
  { label: "Vannaka", names: ["Vannaka"] },
  { label: "Chaeldar", names: ["Chaeldar"] },
  { label: "Konar quo Maten", names: ["Konar quo Maten"] },
  { label: "Nieve", names: ["Nieve", "Steve"] },
  { label: "Duradel", names: ["Duradel", "Kuradal"] },
  { label: "Krystilia", names: ["Krystilia"] },
  { label: "Mortimer", names: ["Mortimer"] },
];

const MASTER_OPTIONS = [
  { value: "", label: "All masters" },
  ...MASTER_GROUPS.map((group) => ({ value: group.names.join(","), label: group.label })),
];

/**
 * Slayer panel's History sub-tab - a paginated, newest-first list of a member's slayer task
 * history (`GET .../get-slayer-task-history`), filterable by status/master via two dropdowns
 * (pill-style filter chips were tried first and rejected during design review). Mounted fresh by
 * `slayer-panel`'s `showTab` on every switch to History, so no state needs to survive a tab
 * switch away and back.
 */
export class SlayerHistoryTab extends BaseElement {
  constructor() {
    super();
    this.entries = [];
    this.page = 1;
    this.totalPages = 1;
    this.loading = false;
    this.status = "";
    this.masterName = "";
  }

  html() {
    return `{{slayer-history-tab.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    this.playerName = this.getAttribute("player-name");
    this.render();

    this.statusSelect = this.querySelector(".slayer-history-tab__status-select");
    this.masterSelect = this.querySelector(".slayer-history-tab__master-select");
    this.listEl = this.querySelector(".slayer-history-tab__list");
    this.pagerStatus = this.querySelector(".slayer-history-tab__pager-status");
    this.firstBtn = this.querySelector(".slayer-history-tab__pager-first");
    this.prevBtn = this.querySelector(".slayer-history-tab__pager-prev");
    this.nextBtn = this.querySelector(".slayer-history-tab__pager-next");
    this.lastBtn = this.querySelector(".slayer-history-tab__pager-last");

    this.statusSelect.innerHTML = STATUS_OPTIONS.map((o) => `<option value="${o.value}">${o.label}</option>`).join("");
    this.masterSelect.innerHTML = MASTER_OPTIONS.map((o) => `<option value="${o.value}">${o.label}</option>`).join("");

    this.eventListener(this.statusSelect, "change", () => this.setFilters({ status: this.statusSelect.value }));
    this.eventListener(this.masterSelect, "change", () => this.setFilters({ masterName: this.masterSelect.value }));
    this.eventListener(this.firstBtn, "click", () => this.goToPage(1));
    this.eventListener(this.prevBtn, "click", () => this.goToPage(this.page - 1));
    this.eventListener(this.nextBtn, "click", () => this.goToPage(this.page + 1));
    this.eventListener(this.lastBtn, "click", () => this.goToPage(this.totalPages));

    // A task closing (or a fresh one starting) while History is the active tab should show up
    // without the member switching tabs and back - refetch the current page under the current
    // filters, since a close/start can shift how many pages exist.
    this.subscribe(`slayerTask:${this.playerName}`, () => this.goToPage(this.page));

    this.goToPage(1);
  }

  disconnectedCallback() {
    super.disconnectedCallback();
  }

  setFilters({ status, masterName }) {
    if (status !== undefined) this.status = status;
    if (masterName !== undefined) this.masterName = masterName;
    this.goToPage(1);
  }

  async goToPage(page) {
    if (this.loading) return;
    this.loading = true;
    this.renderPager();

    const result = await api.getSlayerHistory({
      playerName: this.playerName,
      page,
      status: this.status || undefined,
      masterName: this.masterName || undefined,
    });

    this.entries = result.entries ?? [];
    this.page = result.page ?? 1;
    this.totalPages = result.total_pages ?? 1;
    this.loading = false;
    this.renderList();
  }

  renderList() {
    if (!this.listEl) return;

    if (this.entries.length === 0) {
      this.listEl.innerHTML = `<div class="slayer-history-tab__empty">No tasks match these filters</div>`;
    } else {
      this.listEl.innerHTML = this.entries.map((entry) => this.renderRow(entry)).join("");
    }

    this.renderPager();
  }

  renderPager() {
    if (!this.pagerStatus) return;

    const atFirst = this.page <= 1;
    const atLast = this.page >= this.totalPages;

    this.firstBtn.disabled = this.loading || atFirst;
    this.prevBtn.disabled = this.loading || atFirst;
    this.nextBtn.disabled = this.loading || atLast;
    this.lastBtn.disabled = this.loading || atLast;
    this.pagerStatus.innerHTML = `<strong>${this.page}</strong> / ${this.totalPages}`;
  }

  renderBossBadge(taskName) {
    if (!slayerData.isBossTask(taskName)) return "";
    return `
      <span class="slayer-history-tab__boss-badge" title="Boss task">
        <svg viewBox="0 0 24 24" fill="none">
          <path d="M12 2C7 2 4 5.5 4 10c0 3 1.6 5 3 6.2V19a1 1 0 0 0 1 1h1.5v-2h1v2h3v-2h1v2H16a1 1 0 0 0 1-1v-2.8c1.4-1.2 3-3.2 3-6.2 0-4.5-3-8-8-8Z" fill="currentColor"/>
          <circle cx="9" cy="10" r="1.4" fill="#2a1208"/>
          <circle cx="15" cy="10" r="1.4" fill="#2a1208"/>
        </svg>
        Boss
      </span>
    `;
  }

  renderModifierBadge(entry) {
    const icon = slayerData.modifierIconUrl(entry.modifierType, entry.modifierNegative);
    if (!icon) return "";
    const tooltip = slayerData.modifierTooltip(entry.modifierType, entry.modifierValue, entry.modifierNegative);
    return `
      <span class="slayer-history-tab__modifier" tabindex="0">
        <img src="${icon}" alt="${tooltip}" />
        <span class="slayer-history-tab__modifier-tip">${tooltip}</span>
      </span>
    `;
  }

  renderRow(entry) {
    const meta = STATUS_META[entry.status] ?? { label: entry.status, cls: "ns" };
    const taskIcon = slayerData.taskIconUrl(entry.taskName);
    const masterIcon = slayerData.masterIconUrl(entry.masterName);
    const taskWikiUrl = slayerData.taskWikiUrl(entry.taskName);
    const masterWikiUrl = slayerData.masterWikiUrl(entry.masterName);

    let pointsLabel = "&mdash;";
    let pointsCls = "slayer-history-tab__points slayer-history-tab__points--muted";
    if (entry.points != null) {
      pointsLabel = (entry.points > 0 ? "+" : "") + entry.points;
      pointsCls =
        "slayer-history-tab__points " +
        (entry.points > 0 ? "slayer-history-tab__points--pos" : "slayer-history-tab__points--neg");
    }

    // Open tasks (not_started/in_progress) have no closedAt yet, so show when they were assigned;
    // everything else shows when it closed out.
    const isOpen = OPEN_STATUSES.has(entry.status);
    const timestamp = isOpen ? entry.assignedAt : entry.closedAt ?? entry.assignedAt;
    const timestampDate = new Date(timestamp);
    const timestampLabel = (isOpen ? "assigned " : "") + formatRelativeTimeLong(timestampDate);

    return `
      <div class="slayer-history-tab__row">
        <a class="slayer-history-tab__icon-link" href="${taskWikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${
      entry.taskName
    } on the wiki">
          <img class="slayer-history-tab__icon" src="${taskIcon}" alt="${entry.taskName}" />
        </a>
        <div class="slayer-history-tab__body">
          <div class="slayer-history-tab__top">
            <span class="slayer-history-tab__name-wrap">
              <a class="slayer-history-tab__name" href="${taskWikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${
      entry.taskName
    } on the wiki">${entry.taskName}</a>
              ${this.renderBossBadge(entry.taskName)}
              ${this.renderModifierBadge(entry)}
            </span>
            <span class="${pointsCls}">${pointsLabel}</span>
          </div>
          <div class="slayer-history-tab__bottom">
            <span class="slayer-history-tab__master">
              ${
                masterIcon
                  ? `<a class="slayer-history-tab__master-icon-link" href="${masterWikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${entry.masterName} on the wiki"><img class="slayer-history-tab__master-icon" src="${masterIcon}" alt="${entry.masterName}" /></a>`
                  : ""
              }
              <a class="slayer-history-tab__master-name" href="${masterWikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${
      entry.masterName
    } on the wiki">${entry.masterName}</a>
            </span>
            <span class="slayer-history-tab__kills">${entry.amountDone}/${entry.amountTotal}</span>
            <span class="slayer-history-tab__badge slayer-history-tab__badge--${meta.cls}">${meta.label}</span>
          </div>
          <div class="slayer-history-tab__timestamp" title="${timestampDate.toLocaleString()}">${timestampLabel}</div>
        </div>
      </div>
    `;
  }
}
customElements.define("slayer-history-tab", SlayerHistoryTab);
