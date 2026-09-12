import { BaseElement } from "../base-element/base-element";
import { api } from "../data/api";
import { slayerData } from "../data/slayer";

// Fastest-completion tile wants exact seconds visible (a one-tick turn-in is the interesting
// case), unlike `formatDuration` elsewhere in the site which rounds to whole minutes.
function formatExactDuration(totalSeconds) {
  const seconds = totalSeconds % 60;
  const totalMinutes = Math.floor(totalSeconds / 60);
  const minutes = totalMinutes % 60;
  const totalHours = Math.floor(totalMinutes / 60);
  const hours = totalHours % 24;
  const days = Math.floor(totalHours / 24);

  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m ${seconds}s`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}

/**
 * Slayer panel's Stats sub-tab - all-time slayer task stats for one member
 * (`GET .../get-slayer-task-stats`). Mounted fresh by `slayer-panel`'s `showTab` on every switch
 * to Stats, matching `slayer-history-tab`.
 */
export class SlayerStatsTab extends BaseElement {
  constructor() {
    super();
  }

  html() {
    return `{{slayer-stats-tab.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    this.playerName = this.getAttribute("player-name");
    this.render();

    this.load();
    // A task closing while Stats is the active tab should update the tiles without the member
    // having to switch away and back.
    this.subscribe(`slayerTask:${this.playerName}`, () => this.load());
  }

  disconnectedCallback() {
    super.disconnectedCallback();
  }

  async load() {
    this.stats = await api.getSlayerStats({ playerName: this.playerName });
    this.render();
  }

  renderLeaderTile(leader, label, countSuffix, negative) {
    if (!leader) {
      return `
        <div class="slayer-stats-tab__tile${negative ? " slayer-stats-tab__tile--negative" : ""}">
          <span class="slayer-stats-tab__tile-label">${label}</span>
          <span class="slayer-stats-tab__tile-empty">&mdash;</span>
        </div>
      `;
    }

    const icon = slayerData.taskIconUrl(leader.name);
    const wikiUrl = slayerData.taskWikiUrl(leader.name);
    return `
      <div class="slayer-stats-tab__tile${negative ? " slayer-stats-tab__tile--negative" : ""}">
        <span class="slayer-stats-tab__tile-label">${label}</span>
        <div class="slayer-stats-tab__tile-body">
          <a class="slayer-stats-tab__tile-icon-link" href="${wikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${
      leader.name
    } on the wiki">
            <img class="slayer-stats-tab__tile-icon" src="${icon}" alt="${leader.name}" />
          </a>
          <span>
            <a class="slayer-stats-tab__tile-name" href="${wikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${
      leader.name
    } on the wiki">${leader.name}</a><br />
            <span class="slayer-stats-tab__tile-count">${leader.count.toLocaleString()}${countSuffix}</span>
          </span>
        </div>
      </div>
    `;
  }

  renderMasterTile(leader) {
    if (!leader) {
      return `
        <div class="slayer-stats-tab__tile">
          <span class="slayer-stats-tab__tile-label">Most common master</span>
          <span class="slayer-stats-tab__tile-empty">&mdash;</span>
        </div>
      `;
    }

    const icon = slayerData.masterIconUrl(leader.name);
    const wikiUrl = slayerData.masterWikiUrl(leader.name);
    return `
      <div class="slayer-stats-tab__tile">
        <span class="slayer-stats-tab__tile-label">Most common master</span>
        <div class="slayer-stats-tab__tile-body">
          ${
            icon
              ? `<a class="slayer-stats-tab__tile-icon-link" href="${wikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${leader.name} on the wiki"><img class="slayer-stats-tab__tile-icon slayer-stats-tab__tile-icon--master" src="${icon}" alt="${leader.name}" /></a>`
              : ""
          }
          <span>
            <a class="slayer-stats-tab__tile-name" href="${wikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${
      leader.name
    } on the wiki">${leader.name}</a><br />
            <span class="slayer-stats-tab__tile-count">${leader.count.toLocaleString()} tasks</span>
          </span>
        </div>
      </div>
    `;
  }

  renderModifierTile(leader) {
    if (!leader) {
      return `
        <div class="slayer-stats-tab__tile">
          <span class="slayer-stats-tab__tile-label">Most common modifier</span>
          <span class="slayer-stats-tab__tile-empty">&mdash;</span>
        </div>
      `;
    }

    const icon = slayerData.modifierIconUrl(leader.modifierType, leader.modifierNegative);
    const label = slayerData.modifierLabel(leader.modifierType, leader.modifierNegative) ?? leader.modifierType;
    return `
      <div class="slayer-stats-tab__tile">
        <span class="slayer-stats-tab__tile-label">Most common modifier</span>
        <div class="slayer-stats-tab__tile-body">
          ${
            icon
              ? `<img class="slayer-stats-tab__tile-icon slayer-stats-tab__tile-icon--modifier" src="${icon}" alt="${label}" />`
              : ""
          }
          <span>
            <span class="slayer-stats-tab__tile-name">${label}</span><br />
            <span class="slayer-stats-tab__tile-count">${leader.count.toLocaleString()} times</span>
          </span>
        </div>
      </div>
    `;
  }

  renderFastestTile(leader) {
    if (!leader) {
      return `
        <div class="slayer-stats-tab__tile">
          <span class="slayer-stats-tab__tile-label">Fastest completion</span>
          <span class="slayer-stats-tab__tile-empty">&mdash;</span>
        </div>
      `;
    }

    const icon = slayerData.taskIconUrl(leader.name);
    const wikiUrl = slayerData.taskWikiUrl(leader.name);
    const duration = formatExactDuration(leader.seconds);
    return `
      <div class="slayer-stats-tab__tile">
        <span class="slayer-stats-tab__tile-label">Fastest completion</span>
        <div class="slayer-stats-tab__tile-body">
          <a class="slayer-stats-tab__tile-icon-link" href="${wikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${leader.name} on the wiki">
            <img class="slayer-stats-tab__tile-icon" src="${icon}" alt="${leader.name}" />
          </a>
          <span>
            <a class="slayer-stats-tab__tile-name" href="${wikiUrl}" target="_blank" rel="noopener noreferrer" title="View ${leader.name} on the wiki">${leader.name}</a><br />
            <span class="slayer-stats-tab__tile-count">${duration}</span>
          </span>
        </div>
      </div>
    `;
  }

  renderBody() {
    const s = this.stats;
    if (!s) return `<div class="slayer-stats-tab__loading">Loading&hellip;</div>`;

    return `
      <div class="slayer-stats-tab__totals">
        <div class="slayer-stats-tab__total">
          <span class="slayer-stats-tab__total-n">${s.tasks_completed.toLocaleString()}</span>
          <span class="slayer-stats-tab__total-l">Completed</span>
        </div>
        <div class="slayer-stats-tab__total">
          <span class="slayer-stats-tab__total-n">${s.total_kills.toLocaleString()}</span>
          <span class="slayer-stats-tab__total-l">Kills</span>
        </div>
        <div class="slayer-stats-tab__total">
          <span class="slayer-stats-tab__total-n">${s.total_points_earned.toLocaleString()}</span>
          <span class="slayer-stats-tab__total-l">Points earned</span>
        </div>
      </div>

      <div class="slayer-stats-tab__grid">
        <div class="slayer-stats-tab__tile slayer-stats-tab__tile--wide">
          <span class="slayer-stats-tab__tile-label">Completion rate</span>
          <div class="slayer-stats-tab__rate-row">
            <span class="slayer-stats-tab__rate-n">${s.completion_rate}%</span>
            <div class="slayer-stats-tab__rate-track">
              <div class="slayer-stats-tab__rate-fill" style="width: ${s.completion_rate}%"></div>
            </div>
          </div>
        </div>

        ${this.renderLeaderTile(s.most_killed_task, "Most killed overall", " killed", false)}
        ${this.renderLeaderTile(s.most_common_task, "Most common task", " times", false)}
        ${this.renderMasterTile(s.most_common_master)}
        ${this.renderLeaderTile(s.most_cancelled_task, "Most cancelled", "&times; cancelled", true)}
        ${this.renderModifierTile(s.most_common_modifier)}
        ${this.renderFastestTile(s.fastest_completed_task)}
      </div>
    `;
  }
}
customElements.define("slayer-stats-tab", SlayerStatsTab);
