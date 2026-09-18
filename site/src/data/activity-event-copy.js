import { Quest } from "./quest";
import { Item } from "./item";
import { combatAchievement } from "./combat-achievement";
import { slugifyNpcName } from "./npc-slug";
import { BOSS_ICON_SLUGS } from "./boss-icons";
import { Skill } from "./skill";

const COLLECTION_LOG_WIKI_URL = "https://oldschool.runescape.wiki/w/Collection_log";

// The activity feed and the toast stack render the same six milestone event types, so the copy
// lives here once and each surface just supplies its own member/subject wrappers.
export const ACTIVITY_EVENT_TYPES = [
  "kill",
  "death",
  "quest",
  "diary",
  "combat_task",
  "collection_log",
  "clue",
  "raid",
  "level_up",
];

const BADGE_LABELS = {
  kill: "Kill",
  death: "Death",
  quest: "Quest",
  diary: "Diary",
  combat_task: "Combat task",
  collection_log: "Collection log",
  clue: "Clue",
  // Raid completions display under their own per-raid icon (cox/tob/toa - see
  // `activityDisplayType`), but all three share the one "Raid" badge/filter chip.
  cox: "Raid",
  tob: "Raid",
  toa: "Raid",
  level_up: "Level up",
  // Level 99 is discriminated into its own display type (see `activityDisplayType`) so it can
  // carry a badge that stands out from an ordinary milestone.
  max_level: "99",
};

// Secondary line under a toast title. Kills/deaths speak for themselves, so they have none.
const META_LABELS = {
  diary: "Achievement Diary",
  collection_log: "Collection log",
};

// Clue casket openings are stored as `loot` events (shared with chest openings - see LootEvent in
// models.rs) discriminated by `clueTier` being set, so every surface that wants to treat them as
// their own "clue" bucket has to derive that display type rather than reading `event_type` raw.
export function activityDisplayType(event) {
  if (event.event_type === "loot" && event.payload?.clueTier) return "clue";
  // Raid completions are stored under one shared `event_type: "raid"`, discriminated by
  // `payload.raidType` ("cox"/"tob"/"toa") so each raid can render its own icon/badge.
  if (event.event_type === "raid") return event.payload?.raidType || "raid";
  // Every skill level-up shares one stored `event_type: "level_up"` - level 99 is split into its
  // own display type purely so the badge/icon can call it out as the max-level milestone it is.
  if (event.event_type === "level_up") return event.payload?.level === 99 ? "max_level" : "level_up";
  return event.event_type;
}

export function activityBadgeLabel(event) {
  const displayType = activityDisplayType(event);
  return BADGE_LABELS[displayType] || displayType;
}

export function clueTierLabel(tier) {
  return tier ? tier[0].toUpperCase() + tier.slice(1) : "";
}

export function clueValueFor(payload) {
  return (payload?.loot || []).reduce((total, entry) => total + new Item(entry.item_id).gePrice * entry.quantity, 0);
}

export function clueWikiUrl(tier) {
  return `https://oldschool.runescape.wiki/w/Clue_scroll_(${tier})`;
}

export function questNameFor(payload) {
  const questId = payload?.quest_id;
  return Quest.questData?.[questId]?.name || "a quest";
}

export function combatTaskFor(payload) {
  const task = combatAchievement.taskById(payload?.task_id);
  return {
    name: task?.name || "a combat task",
    tierLabel: task?.tierLabel || null,
  };
}

export function collectionLogItemNameFor(payload) {
  const itemId = payload?.item_id;
  return Item.itemDetails?.[itemId]?.name || "an item";
}

export function activityMetaLabel(event) {
  if (event.event_type === "combat_task") {
    if (event.payload?.kind === "boss") return "Combat achievements";
    const { tierLabel } = combatTaskFor(event.payload);
    return tierLabel ? `${tierLabel} combat task` : "Combat task";
  }
  if (event.event_type === "level_up") return event.payload?.level === 99 ? "Max level!" : "Level up";
  return META_LABELS[event.event_type] || "";
}

// Where a toast/activity-feed row should link when clicked. Most event types don't have a
// dedicated per-item page, so they fall back to the activity feed itself.
const LINK_BY_EVENT_TYPE = {
  combat_task: "/group/combat-achievements",
  kill: "/group/loot-log",
  raid: "/group/loot-log",
};

export function activityLinkFor(event) {
  return LINK_BY_EVENT_TYPE[event.event_type] || "/group/activity";
}

export function npcWikiUrl(npcName) {
  return `https://oldschool.runescape.wiki/w/Special:Lookup?type=npc&name=${encodeURIComponent(npcName)}`;
}

// Repeat kills/deaths against the same boss by the same member within this window collapse into
// one activity-feed row / toast instead of stacking duplicates - see `mergeOrCreateRow` in
// activity-feed-page.js and `handleToast` in toast-stack.js. Each merge resets the window from
// the latest event, so a steady drip keeps merging indefinitely.
export const KILL_MERGE_WINDOW_MS = 60 * 60 * 1000;

export function killGroupKey(event) {
  const npc =
    event.payload?.npcName || event.payload?.npc_name || event.payload?.killerName || event.payload?.killer_name;
  return `${event.member_name}|${npc}`;
}

// Order-independent comparison of two kill payloads' sub_kills (combo bosses only - Barrows/Moons
// of Peril, see `KillEvent::sub_kills` server-side). A combo kill whose sub_kills differ from the
// row/toast it would otherwise fold into (e.g. one Moons attempt killing Eclipse+Blue+Blood, the
// next only Blue) is a distinct kill, not a repeat of the same one - merging it would silently
// drop its own breakdown (only the first event's sub_kills ever got kept) and misreport it as
// "x2" of the earlier combo, so callers should skip merging when this returns false. Kill order
// can legitimately differ between two attempts that still killed the exact same sub-bosses, so
// this only cares about the set, not the sequence.
export function subKillsMatch(payloadA, payloadB) {
  const a = payloadA?.subKills || payloadA?.sub_kills || [];
  const b = payloadB?.subKills || payloadB?.sub_kills || [];
  if (a.length !== b.length) return false;
  const sortedA = [...a].sort();
  const sortedB = [...b].sort();
  return sortedA.every((label, i) => label === sortedB[i]);
}

// Doom of Mokhaiotl only (see `KillEvent::delve_level` server-side). Same reasoning as
// `subKillsMatch`: a kill at a different delve level than the row/toast it would otherwise fold
// into is a distinct run, not a repeat of the same one - merging it would keep only the first
// event's level and misreport a level-1 and a level-4 run as "x2" of one level. Every other kind
// of kill has `delveLevel` absent on both sides, which counts as a match so this never blocks
// ordinary merging.
export function delveLevelMatch(payloadA, payloadB) {
  const a = payloadA?.delveLevel ?? payloadA?.delve_level ?? null;
  const b = payloadB?.delveLevel ?? payloadB?.delve_level ?? null;
  return a === b;
}

// Same self-hosted RuneLite-hiscore-style icon the loot log uses per boss (see
// `loot-log-group.js`'s `iconUrl` getter) - null falls back to no icon for NPCs outside that set.
export function bossIconFor(npcName) {
  if (!npcName) return null;
  const slug = slugifyNpcName(npcName);
  return BOSS_ICON_SLUGS.has(slug) ? `/icons/hiscore/bosses/${slug}.png` : null;
}

// Category icons for the milestone event types - saved locally from the OSRS wiki's own
// icons for Quests, Achievement Diaries, Combat Achievements and the Collection log.
const ACTIVITY_ICONS = {
  quest: "/icons/activity/quest.png",
  diary: "/icons/activity/diary.png",
  combat_task: "/icons/activity/combat-task.png",
  collection_log: "/icons/activity/collection-log.png",
  clue: "/icons/activity/clue.png",
  cox: "/icons/activity/cox.png",
  tob: "/icons/activity/tob.png",
  // ToA has no single icon here - see `raidIconFor`, which picks one of the three below by
  // invocation level, matching the wiki's own per-mode iconography for this raid.
};

const RAID_NAMES = {
  cox: "Chambers of Xeric",
  tob: "Theatre of Blood",
  toa: "Tombs of Amascut",
};

// ToA's invocation-level bands, matching the wiki's own mode split: Entry 0-149, Normal 150-299,
// Expert 300-600. Unlike CoX/ToB (one fixed icon each), ToA's icon depends on which band the
// completed run's level falls into.
function raidIconFor(raidType, payload) {
  if (raidType !== "toa") return ACTIVITY_ICONS[raidType];
  const level = payload?.difficulty?.level || 0;
  if (level >= 300) return "/icons/activity/toa-expert.png";
  if (level >= 150) return "/icons/activity/toa-normal.png";
  return "/icons/activity/toa-entry.png";
}

function joinNames(names) {
  if (names.length <= 1) return names[0] || "";
  if (names.length === 2) return `${names[0]} and ${names[1]}`;
  return `${names.slice(0, -1).join(", ")}, and ${names[names.length - 1]}`;
}

// Sums every reporting member's own share of a merged raid completion's loot (see
// `RaidCompletionPayload` in models.rs) - mirrors `clueValueFor`'s "recompute from loot at
// render time" approach rather than trusting the server-cached `totalValue` blindly.
export function raidValueFor(payload) {
  return (payload?.participants || []).reduce(
    (total, participant) =>
      total + (participant.loot || []).reduce((sum, entry) => sum + new Item(entry.itemId).gePrice * entry.quantity, 0),
    0
  );
}

export function raidDifficultyLabel(payload) {
  const difficulty = payload?.difficulty;
  if (!difficulty) return null;
  if (difficulty.kind === "level") {
    return difficulty.level > 0 ? `level ${difficulty.level}` : "unknown level";
  }
  return difficulty.mode || null;
}

export function questWikiUrl(questName) {
  return `https://oldschool.runescape.wiki/w/${questName.replaceAll(" ", "_")}`;
}

// Matches the wiki's own diary page naming, e.g. https://oldschool.runescape.wiki/w/Ardougne_Diary#Hard
// (same pattern already used by diary-dialog.js for the per-tier section link).
export function diaryWikiUrl(region, tier) {
  return `https://oldschool.runescape.wiki/w/${region.replace(/ /g, "_")}_Diary#${tier}`;
}

export function skillWikiUrl(skillName) {
  return `https://oldschool.runescape.wiki/w/${skillName}`;
}

// Skill-flavored verb phrase each level-up sentence ends on, e.g. "Torvesta chopped their way to
// level 60 Woodcutting" - every skill gets its own so the feed doesn't read as 24 copies of the
// same generic "leveled up" line. Each phrase reads naturally immediately before "level N <Skill>".
const SKILL_LEVEL_UP_VERBS = {
  Agility: "vaulted their way to",
  Attack: "sharpened their blade to",
  Construction: "hammered out",
  Cooking: "cooked their way to",
  Crafting: "crafted their way to",
  Defence: "toughened up to",
  Farming: "harvested their way to",
  Firemaking: "stoked the flames to",
  Fishing: "reeled in",
  Fletching: "carved their way to",
  Herblore: "brewed their way to",
  Hitpoints: "toughed it out to",
  Hunter: "trapped their way to",
  Magic: "conjured their way to",
  Mining: "struck",
  Prayer: "prayed their way to",
  Ranged: "fired their way to",
  Runecraft: "bound their way to",
  Slayer: "slew their way to",
  Smithing: "forged their way to",
  Strength: "muscled up to",
  Thieving: "pickpocketed their way to",
  Woodcutting: "chopped their way to",
  Sailing: "sailed their way to",
};

const identity = (value) => value;

/// Phrasing convention for group-milestone copy across this app: "<member> <verb> <subject>"
/// (e.g. "Bandos killed Vorkath", "Torvesta completed the Kandarin Elite diary") - plain
/// past-tense sentences naming the actual boss/quest/diary/task/item, not a generic
/// "New activity" label. Discord webhook payloads (#35) and low-HP/wilderness alerts (#37-39)
/// should read the same way.
export function activityEventDescription(event, format = {}) {
  const wrapMember = format.member || identity;
  const wrapSubject = format.subject || identity;
  const member = wrapMember(event.member_name);
  const payload = event.payload || {};

  switch (activityDisplayType(event)) {
    case "kill": {
      // `npcName` is the server's camelCase `KillEvent` serialization; `npc_name` is accepted
      // too so either casing renders.
      const npc = payload.npcName || payload.npc_name;
      const noLoot = !payload.loot || payload.loot.length === 0;
      // `aggregateCount` is a client-side field set by the activity feed/toast stack when folding
      // repeat kills/deaths against the same boss by the same member within an hour into one row
      // (see `mergeOrCreateRow` in activity-feed-page.js) - it's never sent by the server.
      const count = event.aggregateCount || 1;
      // Combo kills only (e.g. "Barrows"/"Moons of Peril" - see `KillEvent::sub_kills`
      // server-side): names whichever sub-bosses were actually killed that run, e.g.
      // "Moons of Peril (Eclipse, Blood)".
      const subKills = payload.subKills || payload.sub_kills;
      const subKillsSuffix = subKills?.length ? ` (${subKills.join(", ")})` : "";
      // Doom of Mokhaiotl only (see `KillEvent::delve_level` server-side): the plugin ships one
      // entry per delve run, for the level reached when rewards were claimed - absent if the
      // varp read failed, in which case this just reads like an ordinary kill.
      const delveLevel = payload.delveLevel ?? payload.delve_level;
      const delveLevelSuffix = delveLevel != null ? ` (delve level ${delveLevel})` : "";
      return `${member} killed ${wrapSubject(
        npc || "an NPC",
        "monster",
        npc && npcWikiUrl(npc),
        bossIconFor(npc)
      )}${subKillsSuffix}${delveLevelSuffix}${count > 1 ? ` &times;${count}` : ""}${noLoot ? " — no loot" : ""}`;
    }
    case "death": {
      const killer = payload.killerName || payload.killer_name;
      const count = event.aggregateCount || 1;
      const countSuffix = count > 1 ? ` &times;${count}` : "";
      // Doom of Mokhaiotl only (see `DeathEvent::doom_delve_level` server-side): the delve level
      // read live off the current-level varp at the moment of death. Absent outside a delve run.
      const doomDelveLevel = payload.doomDelveLevel ?? payload.doom_delve_level;
      const levelSuffix = doomDelveLevel != null ? ` (on level ${doomDelveLevel})` : "";
      return killer
        ? `${member} died to ${wrapSubject(killer, "death", npcWikiUrl(killer))}${levelSuffix}${countSuffix}`
        : `${member} died${levelSuffix}${countSuffix}`;
    }
    case "quest": {
      const questName = questNameFor(payload);
      return `${member} completed ${wrapSubject(questName, "quest", questWikiUrl(questName), ACTIVITY_ICONS.quest)}`;
    }
    case "diary":
      return `${member} completed the ${wrapSubject(
        `${payload.region} ${payload.tier}`,
        "diary",
        diaryWikiUrl(payload.region, payload.tier),
        ACTIVITY_ICONS.diary
      )} diary`;
    case "combat_task": {
      if (payload.kind === "boss") {
        return `${member} completed all combat achievements for ${wrapSubject(
          payload.boss,
          "combat_task",
          combatAchievement.bossWikiUrl(payload.boss),
          ACTIVITY_ICONS.combat_task
        )}`;
      }
      const { name, tierLabel } = combatTaskFor(payload);
      return `${member} completed ${wrapSubject(
        name,
        "combat_task",
        combatAchievement.taskWikiUrl(name),
        ACTIVITY_ICONS.combat_task
      )}${tierLabel ? ` (${tierLabel})` : ""}`;
    }
    case "collection_log":
      if (payload.kind === "page") {
        return `${member} completed the ${wrapSubject(
          payload.page,
          "collection_log",
          COLLECTION_LOG_WIKI_URL,
          ACTIVITY_ICONS.collection_log
        )} collection log`;
      }
      return `${member} added ${wrapSubject(
        collectionLogItemNameFor(payload),
        "collection_log",
        new Item(payload.item_id).wikiLink,
        Item.itemDetails?.[payload.item_id] ? Item.imageUrl(payload.item_id) : null,
        ACTIVITY_ICONS.collection_log
      )} to their collection log`;
    case "clue": {
      const tierLabel = clueTierLabel(payload.clueTier);
      const article = /^[aeiou]/i.test(tierLabel) ? "an" : "a";
      const value = clueValueFor(payload);
      return `${member} completed ${article} ${wrapSubject(
        `${tierLabel} clue`,
        "clue",
        clueWikiUrl(payload.clueTier),
        ACTIVITY_ICONS.clue
      )} — worth ${value.toLocaleString()} gp`;
    }
    case "level_up":
    case "max_level": {
      const skillName = payload.skill;
      const level = payload.level;
      const verb = SKILL_LEVEL_UP_VERBS[skillName] || "leveled up to";
      const subject = wrapSubject(
        `level ${level} ${skillName}`,
        "level_up",
        skillWikiUrl(skillName),
        Skill.getIcon(skillName)
      );
      return level === 99 ? `${member} ${verb} ${subject} — maxed!` : `${member} ${verb} ${subject}`;
    }
    case "cox":
    case "tob":
    case "toa": {
      const raidType = activityDisplayType(event);
      const raidName = RAID_NAMES[raidType];
      const participants = payload.participants || [];
      const names = participants.length ? participants.map((p) => p.memberName) : [event.member_name];
      // Every other branch above wraps a single RSN through `wrapMember` - here the joined list
      // stands in for "the member" since a merged raid completion has more than one, which is
      // why this doesn't just reuse the `member` binding computed at the top of this function.
      const nameList = wrapMember(joinNames(names));
      const total = raidValueFor(payload);
      const diffLabel = raidDifficultyLabel(payload);
      const suffix = diffLabel ? ` (${diffLabel})` : "";
      const gpSuffix = names.length > 1 ? "gp total" : "gp";
      return `${nameList} completed ${wrapSubject(
        raidName,
        raidType,
        npcWikiUrl(raidName),
        raidIconFor(raidType, payload)
      )}${suffix} — worth ${total.toLocaleString()} ${gpSuffix}`;
    }
    default:
      return `${member} — ${event.event_type}`;
  }
}
