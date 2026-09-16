// Headshot/portrait icons for the current real OSRS slayer masters, downloaded from the OSRS
// wiki (Special:FilePath/<Master name>.png) and bundled locally under
// /icons/slayer/masters/<key>.png - see CLAUDE.md's asset-sourcing convention (no runtime
// wiki hotlinking).
const SLAYER_MASTER_ICONS = {
  turael: "turael",
  spria: "spria",
  mazchna: "mazchna",
  vannaka: "vannaka",
  chaeldar: "chaeldar",
  nieve: "nieve",
  steve: "steve",
  duradel: "duradel",
  "konar quo maten": "konar-quo-maten",
  krystilia: "krystilia",
  mortimer: "mortimer",
  aya: "aya",
  achtryn: "achtryn",
  kuradal: "kuradal",
};

// Task target (regular monster or assignable boss) -> local icon key, under
// /icons/slayer/monsters/<key>.png. Covers the full slayer task pool; anything not listed here
// falls back to UNKNOWN_TASK_ICON.
const SLAYER_MONSTER_ICONS = {
  "aberrant spectres": "aberrant-spectres",
  "abyssal demons": "abyssal-demons",
  ankou: "ankou",
  aquanites: "aquanites",
  araxytes: "araxytes",
  aviansies: "aviansies",
  bandits: "bandits",
  banshees: "banshees",
  basilisks: "basilisks",
  bats: "bats",
  bears: "bears",
  birds: "birds",
  "black demons": "black-demons",
  "black dragons": "black-dragons",
  "black knights": "black-knights",
  bloodveld: "bloodveld",
  "blue dragons": "blue-dragons",
  "brine rats": "brine-rats",
  "bronze dragons": "bronze-dragons",
  catablepon: "catablepon",
  "cave bugs": "cave-bugs",
  "cave crawlers": "cave-crawlers",
  "cave horrors": "cave-horrors",
  "cave kraken": "cave-kraken",
  "cave slimes": "cave-slimes",
  "chaos druids": "chaos-druids",
  cockatrices: "cockatrices",
  cows: "cows",
  crabs: "crabs",
  "crawling hands": "crawling-hands",
  crocodiles: "crocodiles",
  "custodian stalkers": "custodian-stalkers",
  dagannoth: "dagannoth",
  "dark beasts": "dark-beasts",
  "dark warriors": "dark-warriors",
  dogs: "dogs",
  drakes: "drakes",
  "dust devils": "dust-devils",
  dwarves: "dwarves",
  "earth warriors": "earth-warriors",
  elves: "elves",
  ents: "ents",
  "fever spiders": "fever-spiders",
  "fire giants": "fire-giants",
  "flesh crawlers": "flesh-crawlers",
  "fossil island wyverns": "fossil-island-wyverns",
  "frost dragons": "frost-dragons",
  gargoyles: "gargoyles",
  ghosts: "ghosts",
  ghouls: "ghouls",
  goblins: "goblins",
  "greater demons": "greater-demons",
  "green dragons": "green-dragons",
  gryphons: "gryphons",
  "harpie bug swarms": "harpie-bug-swarms",
  hellhounds: "hellhounds",
  "hill giants": "hill-giants",
  hobgoblins: "hobgoblins",
  hydras: "hydras",
  "ice giants": "ice-giants",
  "ice warriors": "ice-warriors",
  icefiends: "icefiends",
  "infernal mages": "infernal-mages",
  "iron dragons": "iron-dragons",
  jellies: "jellies",
  "jungle horrors": "jungle-horrors",
  kalphites: "kalphites",
  killerwatts: "killerwatts",
  kurasks: "kurasks",
  "lava dragons": "lava-dragons",
  "lesser demons": "lesser-demons",
  "lesser nagua": "lesser-nagua",
  lizardmen: "lizardmen",
  lizards: "lizards",
  "magic axes": "magic-axes",
  mammoths: "mammoths",
  "metal dragons": "metal-dragons",
  minotaurs: "minotaurs",
  "mithril dragons": "mithril-dragons",
  mogres: "mogres",
  molanisks: "molanisks",
  monkeys: "monkeys",
  "moss giants": "moss-giants",
  nechryael: "nechryael",
  ogres: "ogres",
  "otherworldly beings": "otherworldly-beings",
  pirates: "pirates",
  pyrefiends: "pyrefiends",
  rats: "rats",
  "red dragons": "red-dragons",
  revenants: "revenants",
  rockslugs: "rockslugs",
  rogues: "rogues",
  scabarites: "scabarites",
  scorpions: "scorpions",
  "sea snakes": "sea-snakes",
  shades: "shades",
  "shadow warriors": "shadow-warriors",
  "skeletal wyverns": "skeletal-wyverns",
  skeletons: "skeletons",
  "smoke devils": "smoke-devils",
  sourhogs: "sourhogs",
  spiders: "spiders",
  "spiritual creatures": "spiritual-creatures",
  "steel dragons": "steel-dragons",
  suqahs: "suqahs",
  "terror dogs": "terror-dogs",
  trolls: "trolls",
  turoth: "turoth",
  tzhaar: "tzhaar",
  vampyres: "vampyres",
  venators: "venators",
  "wall beasts": "wall-beasts",
  "warped creatures": "warped-creatures",
  waterfiends: "waterfiends",
  werewolves: "werewolves",
  wolves: "wolves",
  wyrms: "wyrms",
  zombies: "zombies",
  zygomites: "zygomites",
  "mutated zygomites": "mutated-zygomites",

  // Every boss assignable via the generic "Boss" slayer task (the task unlocked by the 200-point
  // "Like a boss" reward, given by Duradel/Kuradal, Konar, Nieve/Steve, and Krystilia) - see
  // GroupScapeTrackerPlugin#resolveBossTaskId. Same render-icon style as every other task above,
  // downloaded from the wiki - not the smaller Hiscore-style icon used elsewhere on the site
  // (boss-icons.js's BOSS_ICON_SLUGS).
  "the leviathan": "the-leviathan",
  "the whisperer": "the-whisperer",
  vardorvis: "vardorvis",
  "duke sucellus": "duke-sucellus",
  "abyssal sire": "abyssal-sire",
  "alchemical hydra": "alchemical-hydra",
  cerberus: "cerberus",
  "thermonuclear smoke devil": "thermonuclear-smoke-devil",
  kraken: "kraken",
  "grotesque guardians": "grotesque-guardians",
  "dagannoth rex": "dagannoth-rex",
  "dagannoth prime": "dagannoth-prime",
  "dagannoth supreme": "dagannoth-supreme",
  "kalphite queen": "kalphite-queen",
  "giant mole": "giant-mole",
  sarachnis: "sarachnis",
  "k'ril tsutsaroth": "kril-tsutsaroth",
  "kree'arra": "kreearra",
  "commander zilyana": "commander-zilyana",
  "general graardor": "general-graardor",
  "vet'ion": "vetion",
  callisto: "callisto",
  venenatis: "venenatis",
  scorpia: "scorpia",
  "chaos elemental": "chaos-elemental",
  "chaos fanatic": "chaos-fanatic",
  "crazy archaeologist": "crazy-archaeologist",
  "king black dragon": "king-black-dragon",
  vorkath: "vorkath",
  zulrah: "zulrah",
  "phantom muspah": "phantom-muspah",
  araxxor: "araxxor",
};

const UNKNOWN_TASK_ICON = "/icons/slayer/monsters/unknown-task.png";

// Mortimer task modifiers - see server's SlayerTask::modifier_type doc comment. Only "quantity"
// has an up/down pair (task size can go either way); the other 4 are always positive boosts, so
// they share one icon regardless of modifierNegative.
const MODIFIER_ICONS = {
  points: "/icons/slayer/modifiers/points.png",
  quantity: { up: "/icons/slayer/modifiers/quantity-up.png", down: "/icons/slayer/modifiers/quantity-down.png" },
  clue_rate: "/icons/slayer/modifiers/clue-rate.png",
  superior_rate: "/icons/slayer/modifiers/superior-rate.png",
  xp: "/icons/slayer/modifiers/xp.png",
};

function normalize(name) {
  return `${name ?? ""}`.trim().toLowerCase();
}

// The plugin resolves task names off the game's own DB tables (see SlayerTaskState#resolveTaskName),
// which don't consistently use the same plural/singular form as our icon keys - e.g. the
// "Cockatrices" task resolves to the singular "Cockatrice". Try the exact name first, then the
// pluralized/singularized form, before giving up and falling back to UNKNOWN_TASK_ICON.
function resolveMonsterKey(taskName) {
  const name = normalize(taskName);
  if (SLAYER_MONSTER_ICONS[name]) return SLAYER_MONSTER_ICONS[name];
  if (SLAYER_MONSTER_ICONS[`${name}s`]) return SLAYER_MONSTER_ICONS[`${name}s`];
  if (name.endsWith("s") && SLAYER_MONSTER_ICONS[name.slice(0, -1)]) {
    return SLAYER_MONSTER_ICONS[name.slice(0, -1)];
  }
  return null;
}

function wikiTitle(name) {
  return encodeURIComponent(`${name}`.trim().replace(/\s+/g, "_"));
}

// Some tasks name a group of creatures rather than a single wiki-titled monster (e.g. "Warped
// creatures" covers warped jellies/terrorbirds/tortoises/etc), so the plain task-name URL
// 404s/redirects wrong. The wiki keeps a dedicated Slayer_task/<Task> page for these instead.
const TASK_WIKI_OVERRIDES = {
  "warped creatures": "Slayer_task/Warped_creatures",
  "aberrant spectres": "Aberrant_spectre",
};

class SlayerData {
  masterIconUrl(masterName) {
    const key = SLAYER_MASTER_ICONS[normalize(masterName)];
    return key ? `/icons/slayer/masters/${key}.png` : null;
  }

  taskIconUrl(taskName) {
    const key = resolveMonsterKey(taskName);
    return key ? `/icons/slayer/monsters/${key}.png` : UNKNOWN_TASK_ICON;
  }

  taskWikiUrl(taskName) {
    const override = TASK_WIKI_OVERRIDES[normalize(taskName)];
    if (override) return `https://oldschool.runescape.wiki/w/${override}`;
    return `https://oldschool.runescape.wiki/w/${wikiTitle(taskName)}`;
  }

  masterWikiUrl(masterName) {
    return `https://oldschool.runescape.wiki/w/${wikiTitle(masterName)}`;
  }

  // `null` for a task with no modifier.
  modifierIconUrl(modifierType, modifierNegative) {
    const icons = MODIFIER_ICONS[modifierType];
    if (!icons) return null;
    return typeof icons === "string" ? icons : modifierNegative ? icons.down : icons.up;
  }

  modifierTooltip(modifierType, modifierValue, modifierNegative) {
    switch (modifierType) {
      case "points":
        return `Task awards +${modifierValue} Slayer points`;
      case "quantity": {
        const direction = modifierNegative ? "decreased" : "increased";
        const sign = modifierNegative ? "−" : "+";
        return `Task size ${direction} (${sign}${modifierValue})`;
      }
      case "clue_rate":
        return `+${modifierValue}% clue scroll chance`;
      case "superior_rate":
        return `+${modifierValue}% superior unique chance`;
      case "xp":
        return `+${modifierValue}% Slayer XP`;
      default:
        return null;
    }
  }

  // Short label for the Stats tab's "Most common modifier" tile - no value/percent (that varies
  // task-to-task and isn't what's being counted), just which of the 5 modifier kinds it is.
  modifierLabel(modifierType, modifierNegative) {
    switch (modifierType) {
      case "points":
        return "Points bonus";
      case "quantity":
        return modifierNegative ? "Smaller task" : "Bigger task";
      case "clue_rate":
        return "Clue rate boost";
      case "superior_rate":
        return "Superior rate boost";
      case "xp":
        return "XP boost";
      default:
        return null;
    }
  }
}

export const slayerData = new SlayerData();
