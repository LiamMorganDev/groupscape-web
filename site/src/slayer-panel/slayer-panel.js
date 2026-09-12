import { BaseElement } from "../base-element/base-element";
import { slayerData } from "../data/slayer";

/**
 * Minibar tab (same swap-into-content pattern as `player-inventory`/`player-stats`/etc, see
 * `player-panel`'s `handleMiniBarClick`) showing a group member's slayer info across three
 * sub-tabs: Current task (this component's own markup, unchanged since before the sub-tabs
 * existed), History (`slayer-history-tab`) and Stats (`slayer-stats-tab`) - both separate
 * custom elements, matching `collection-log`/`collection-log-tab`'s parent/child split so
 * switching sub-tabs only swaps `.slayer-panel__tab-content`'s contents rather than wiping this
 * whole element's DOM (and therefore its own tab-strip state) on every re-render.
 */
export class SlayerPanel extends BaseElement {
  constructor() {
    super();
    this.activeTab = "current";
  }

  html() {
    return `{{slayer-panel.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    this.playerName = this.getAttribute("player-name");
    this.subscribeOnce("get-group-data", this.init.bind(this));
    // Only ever repaints the Current tab's own content - see renderCurrentTabIfActive. Kept as a
    // long-lived subscription (rather than one set up/torn down per tab switch) since it also
    // needs to fire while History/Stats are showing, so a task closing while the user is looking
    // at History/Stats is reflected the moment they switch back to Current.
    this.subscribe(`slayerTask:${this.playerName}`, this.renderCurrentTabIfActive.bind(this));
  }

  disconnectedCallback() {
    super.disconnectedCallback();
  }

  init(groupData) {
    this.member = groupData.members.get(this.playerName);
    this.render();

    this.tabButtons = this.querySelector(".slayer-panel__tabs");
    this.tabContent = this.querySelector(".slayer-panel__tab-content");
    this.eventListener(this.tabButtons, "click", this.handleTabClick.bind(this));
    this.showTab(this.activeTab);
  }

  handleTabClick(event) {
    const tabId = event?.target?.getAttribute("tab-id");
    if (tabId) {
      this.showTab(tabId);
    }
  }

  showTab(tabId) {
    this.activeTab = tabId;
    this.tabButtons.querySelectorAll("button[tab-id]").forEach((button) => {
      button.classList.toggle("slayer-panel__tab--active", button.getAttribute("tab-id") === tabId);
    });

    if (tabId === "current") {
      this.renderCurrentTab();
    } else if (tabId === "history") {
      this.tabContent.innerHTML = `<slayer-history-tab player-name="${this.playerName}"></slayer-history-tab>`;
    } else if (tabId === "stats") {
      this.tabContent.innerHTML = `<slayer-stats-tab player-name="${this.playerName}"></slayer-stats-tab>`;
    }
  }

  renderCurrentTabIfActive() {
    if (this.activeTab === "current" && this.tabContent) {
      this.renderCurrentTab();
    }
  }

  renderCurrentTab() {
    this.tabContent.innerHTML = `
      <div class="slayer-panel__stats-row">
        <div class="slayer-panel__streak-ledger">
          ${this.renderStreakCell("normal", "Normal", this.streakNormal())}
          ${this.renderStreakCell("mortimer", "Mortimer", this.streakMortimer())}
          ${this.renderStreakCell("wildy", "Wildy", this.streakWildy())}
        </div>
        <div class="slayer-panel__points-box">
          <span class="slayer-panel__stat-label">Points</span>
          <span class="slayer-panel__stat-value">${this.points()}</span>
        </div>
      </div>

      ${this.hasTask() ? this.renderTask() : this.renderNoTask()}
    `;
  }

  hasTask() {
    return !!this.member?.slayerTask?.hasTask;
  }

  points() {
    return this.member?.slayerTask?.points ?? 0;
  }

  // `null`/`undefined` means that bucket has never been observed for this member yet (as
  // opposed to a real streak of 0) - rendered as "-" rather than 0, see renderStreakCell.
  streakNormal() {
    return this.member?.slayerTask?.streakNormal ?? null;
  }

  streakMortimer() {
    return this.member?.slayerTask?.streakMortimer ?? null;
  }

  streakWildy() {
    return this.member?.slayerTask?.streakWildy ?? null;
  }

  // Which bucket the member's *current* task belongs to, for highlighting the matching cell -
  // `null` while there's no active task, since masterName (and therefore the bucket) is only
  // known while one is assigned.
  activeStreakBucket() {
    if (!this.hasTask()) return null;
    const masterName = (this.member?.slayerTask?.masterName ?? "").trim().toLowerCase();
    if (masterName === "krystilia") return "wildy";
    if (masterName === "mortimer") return "mortimer";
    return "normal";
  }

  amountDone() {
    const task = this.member.slayerTask;
    return Math.max(0, task.initialAmount - task.amountRemaining);
  }

  progressPercent() {
    const task = this.member.slayerTask;
    if (!task.initialAmount) return 0;
    return Math.max(0, Math.min(100, Math.round((this.amountDone() / task.initialAmount) * 100)));
  }

  // Task is still assigned (hasTask) but fully killed - the player hasn't turned it in yet.
  isTaskComplete() {
    return this.member.slayerTask.amountRemaining <= 0;
  }

  renderNoTask() {
    return `<div class="slayer-panel__no-task">No task</div>`;
  }

  renderStreakCell(bucket, label, value) {
    const isActive = this.activeStreakBucket() === bucket;
    return `
      <div class="slayer-panel__streak-cell${isActive ? " slayer-panel__streak-cell--active" : ""}">
        <span class="slayer-panel__stat-label">${label}</span>
        <span class="slayer-panel__stat-value">${
          value ?? '<span class="slayer-panel__stat-value--empty">&mdash;</span>'
        }</span>
      </div>
    `;
  }

  renderMaster() {
    const task = this.member.slayerTask;
    if (!task.masterName) return "";

    const masterIcon = slayerData.masterIconUrl(task.masterName);
    return `
      <a
        class="slayer-panel__master-banner"
        href="${slayerData.masterWikiUrl(task.masterName)}"
        target="_blank"
        rel="noopener"
      >
        ${
          masterIcon
            ? `<img class="slayer-panel__master-chathead" src="${masterIcon}" alt="${task.masterName}" />`
            : `<span class="slayer-panel__master-chathead slayer-panel__master-chathead--fallback"></span>`
        }
        <span class="slayer-panel__master-meta">
          <span class="slayer-panel__master-label">Slayer master</span>
          <span class="slayer-panel__master-name">${task.masterName}</span>
        </span>
      </a>
    `;
  }

  renderModifierBadge(task) {
    const icon = slayerData.modifierIconUrl(task.modifierType, task.modifierNegative);
    if (!icon) return "";
    const tooltip = slayerData.modifierTooltip(task.modifierType, task.modifierValue, task.modifierNegative);
    return `
      <span class="slayer-panel__modifier" tabindex="0">
        <img src="${icon}" alt="${tooltip}" />
        <span class="slayer-panel__modifier-tip">${tooltip}</span>
      </span>
    `;
  }

  renderTask() {
    const task = this.member.slayerTask;
    const complete = this.isTaskComplete();
    // The plugin can occasionally fail to resolve a task's name off the game's own DB tables
    // (see SlayerTaskState#resolveTaskName) while amountRemaining/initialAmount still come
    // through fine - fall back to a placeholder rather than literally printing "null".
    const taskName = task.taskName ?? "Unknown task";
    const taskIcon = slayerData.taskIconUrl(task.taskName);
    const wikiUrl = task.taskName ? slayerData.taskWikiUrl(task.taskName) : null;

    const taskTag = wikiUrl ? "a" : "div";
    const taskClass = `slayer-panel__task${complete ? " slayer-panel__task--complete" : ""}`;
    const taskAttrs = wikiUrl
      ? `href="${wikiUrl}" target="_blank" rel="noopener" class="${taskClass} slayer-panel__task--clickable"`
      : `class="${taskClass}"`;

    return `
      ${this.renderMaster()}

      <${taskTag} ${taskAttrs}>
        <div class="slayer-panel__task-icon-wrap">
          <img class="slayer-panel__task-icon" src="${taskIcon}" alt="${taskName}" />
          ${complete ? `<span class="slayer-panel__task-check">&#10003;</span>` : ""}
        </div>
        <div class="slayer-panel__task-body">
          <span class="slayer-panel__task-name">${taskName}${this.renderModifierBadge(task)}</span>
          ${task.taskLocation ? `<span class="slayer-panel__task-location">${task.taskLocation}</span>` : ""}
          ${wikiUrl ? `<span class="slayer-panel__wiki-link">View slayer guide &#8599;</span>` : ""}
        </div>
      </${taskTag}>

      <div class="slayer-panel__progress">
        <div class="slayer-panel__progress-track${complete ? " slayer-panel__progress-track--complete" : ""}">
          <div class="slayer-panel__progress-fill${
            complete ? " slayer-panel__progress-fill--complete" : ""
          }" style="width: ${this.progressPercent()}%"></div>
        </div>
        <span class="slayer-panel__progress-frac${
          complete ? " slayer-panel__progress-frac--complete" : ""
        }">${this.amountDone()}/${task.initialAmount}</span>
      </div>

      ${
        complete
          ? `
        <div class="slayer-panel__complete-banner">
          <span class="slayer-panel__complete-msg">Task complete</span>
          <span class="slayer-panel__complete-sub">Return to ${
            task.masterName ?? "your slayer master"
          } for a new assignment</span>
        </div>
      `
          : ""
      }
    `;
  }
}
customElements.define("slayer-panel", SlayerPanel);
