import { BaseElement } from "../base-element/base-element";
import { Item } from "../data/item";
import { slugifyNpcName } from "../data/npc-slug";
import { BOSS_ICON_SLUGS, CLUE_TIER_ICONS } from "../data/boss-icons";
import { BOSS_COMBAT_LEVELS } from "../data/boss-levels";
import { timeBounds, formatTimeRange } from "../data/time-range";

const COUNT_LABELS = {
  kill: "kills",
  chest: "opens",
  clue: "caskets",
};

const COUNT_LABELS_SINGULAR = {
  kill: "kill",
  chest: "open",
  clue: "casket",
};

// One farming-session "entry" - consecutive same-member/source/type events <=45min apart (see
// loot-log-page.js's `appendEvent`/`prependEvent`), not a boss-wide group across the whole scope
// like this component used to render.
export class LootLogGroup extends BaseElement {
  constructor() {
    super();
  }

  html() {
    return `{{loot-log-group.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    this.update();
  }

  // Callable again (not just on first mount) so loot-log-page.js can extend this entry with a
  // newly-merged event in place, instead of tearing down and recreating the component.
  update() {
    this.setAttribute("data-source", this.group.sourceType);
    if (this.group.sourceType === "clue" && this.group.clueTier) {
      this.style.setProperty("--clue-tier-color", `var(--clue-${this.group.clueTier})`);
    }
    this.render();
    this.buildTiles();
  }

  buildTiles() {
    const grid = this.querySelector(".loot-log-group__grid");
    grid.innerHTML = "";
    for (const item of this.mergedItems()) {
      const tile = document.createElement("loot-log-tile");
      tile.item = item;
      grid.appendChild(tile);
    }
  }

  // Server sends one row per (event, item); items unrecognized by the client's item table (no
  // name/image/value to render) are dropped rather than shown blank, and rows for the same item
  // across the entry's merged events are folded into one tile with a summed quantity. `matched`
  // is OR'd across contributing events - undefined (no search active) stays undefined.
  mergedItems() {
    const merged = new Map();
    for (const event of this.group.events) {
      for (const item of event.items) {
        if (!Item.exists(item.item_id)) continue;
        if (!merged.has(item.item_id)) {
          merged.set(item.item_id, { ...item, quantity: 0, total_value: 0, matched: undefined });
        }
        const entry = merged.get(item.item_id);
        entry.quantity += item.quantity;
        entry.total_value += item.total_value;
        if (item.matched !== null && item.matched !== undefined) {
          entry.matched = entry.matched || item.matched;
        }
      }
    }
    return [...merged.values()];
  }

  disconnectedCallback() {
    super.disconnectedCallback();
  }

  get sourceLabel() {
    const group = this.group;
    if (group.sourceType === "clue") {
      const tier = group.clueTier ? group.clueTier[0].toUpperCase() + group.clueTier.slice(1) : "";
      return `${tier} clue casket`.trim();
    }
    if (group.sourceType === "kill") {
      // Doom of Mokhaiotl only (see entryKey's delve_level fold-in) - this entry is always a
      // single delve level's worth of kills, so name it the same way the Activity Feed and
      // Discord already write it, rather than the unrelated "(level-X)" combat-level suffix below.
      if (group.delveLevel != null) return `${group.sourceName} (delve level ${group.delveLevel})`;
      const level = BOSS_COMBAT_LEVELS[slugifyNpcName(group.sourceName)];
      if (level) return `${group.sourceName} (level-${level})`;
    }
    return group.sourceName;
  }

  get countLabel() {
    return COUNT_LABELS[this.group.sourceType] || "events";
  }

  get countLabelSingular() {
    return COUNT_LABELS_SINGULAR[this.group.sourceType] || "event";
  }

  // Span of this session entry alone (first event -> last event, merged within the 45-minute
  // window) - a boss farmed for hours shows that whole span, not just an event count.
  get timeRangeLabel() {
    const { first, last } = timeBounds(this.group.events);
    return formatTimeRange(first, last);
  }

  // Self-hosted RuneLite-hiscore-style icon for this group's source (see data/boss-icons.js) -
  // null falls back to the plain colored source dot rendered in loot-log-group.html.
  get iconUrl() {
    const group = this.group;
    if (group.sourceType === "clue") {
      return group.clueTier && CLUE_TIER_ICONS.has(group.clueTier)
        ? `/icons/hiscore/clues/${group.clueTier}.png`
        : null;
    }
    const slug = slugifyNpcName(group.sourceName);
    return BOSS_ICON_SLUGS.has(slug) ? `/icons/hiscore/bosses/${slug}.png` : null;
  }

  // Wiki page for this group's source, following the same conventions used elsewhere in the app
  // (activity-event-copy.js for kills, collection-log-page.js for clues): kills go through the
  // NPC lookup redirect since the same monster name can have multiple wiki pages by combat level;
  // clues and chests map straight to a page title since both are unambiguous exact names.
  get wikiLink() {
    const group = this.group;
    if (group.sourceType === "clue" && group.clueTier) {
      return `https://oldschool.runescape.wiki/w/Clue_scroll_(${group.clueTier})`;
    }
    if (group.sourceType === "kill") {
      return `https://oldschool.runescape.wiki/w/Special:Lookup?type=npc&name=${encodeURIComponent(group.sourceName)}`;
    }
    return `https://oldschool.runescape.wiki/w/${group.sourceName.replace(/ /g, "_")}`;
  }

  get totalValue() {
    return this.mergedItems().reduce((sum, item) => sum + item.total_value, 0);
  }

  get averageValue() {
    return Math.round(this.totalValue / this.group.events.length);
  }
}
customElements.define("loot-log-group", LootLogGroup);
