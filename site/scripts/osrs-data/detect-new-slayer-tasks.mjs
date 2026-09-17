// Reports OSRS Wiki slayer task names not yet represented in slayer.js's SLAYER_MONSTER_ICONS,
// for manual triage. Read-only: writes a report file but never touches slayer.js or the server's
// slayer_boss_tasks.rs (which mirrors the ~35 boss entries at the bottom of that same map per its
// own comment - a candidate accepted here should be added to both, in sync).
//
// Source: every page transcluding {{HasTask|<task name>}} (list=embeddedin on Template:HasTask) -
// this is the wiki's own canonical link from a monster's page to the exact slayer task name it
// belongs to, avoiding the mess of "Slayer task/<name>" subpages (which include stale redirects,
// case variants, and typos and aren't reliably diffable). Monster pages are fetched in batches of
// 50 via prop=revisions instead of one parse call each, since ~300 monster pages embed the
// template.
import axios from "axios";
import fs from "fs/promises";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SITE_ROOT = path.resolve(__dirname, "..", "..");
const SLAYER_DATA_PATH = path.join(SITE_ROOT, "src", "data", "slayer.js");
const REPORT_PATH = path.join(__dirname, "new-slayer-tasks-report.json");

const USER_AGENT = "GroupScape-DataPipeline/1.0 (contact: repo issue tracker)";
const WIKI_API = "https://oldschool.runescape.wiki/api.php";
const BATCH_SIZE = 50;

async function getKnownTaskNames() {
  const raw = await fs.readFile(SLAYER_DATA_PATH, "utf-8");
  const match = raw.match(/const SLAYER_MONSTER_ICONS = \{([\s\S]*?)\n\};/);
  if (!match) throw new Error("Could not find SLAYER_MONSTER_ICONS in slayer.js");
  const keys = [...match[1].matchAll(/^\s*(?:"([^"]+)"|([a-z0-9_]+)):/gm)].map((m) => m[1] ?? m[2]);
  return new Set(keys);
}

async function fetchHasTaskPages() {
  const pages = [];
  let eicontinue;
  do {
    const res = await axios.get(WIKI_API, {
      headers: { "User-Agent": USER_AGENT },
      params: {
        action: "query",
        list: "embeddedin",
        eititle: "Template:HasTask",
        einamespace: 0,
        eilimit: 500,
        format: "json",
        ...(eicontinue ? { eicontinue } : {}),
      },
    });
    const data = res.data;
    const batch = data?.query?.embeddedin;
    if (!Array.isArray(batch)) {
      throw new Error(`Unexpected embeddedin response shape: ${JSON.stringify(data).slice(0, 200)}`);
    }
    pages.push(...batch);
    eicontinue = data?.continue?.eicontinue;
  } while (eicontinue);
  return pages;
}

async function fetchTaskNamesForBatch(titles) {
  const res = await axios.get(WIKI_API, {
    headers: { "User-Agent": USER_AGENT },
    params: {
      action: "query",
      prop: "revisions",
      rvprop: "content",
      rvsection: 0,
      titles: titles.join("|"),
      format: "json",
    },
  });
  const pages = res.data?.query?.pages;
  if (!pages) throw new Error(`Unexpected revisions response shape: ${JSON.stringify(res.data).slice(0, 200)}`);

  const results = [];
  for (const page of Object.values(pages)) {
    const content = page.revisions?.[0]?.["*"];
    if (!content) continue;
    const match = content.match(/\{\{HasTask\|([^}|]+)/);
    if (match) results.push({ monster: page.title, taskName: match[1].trim() });
  }
  return results;
}

// Mirrors slayer.js's own iconForSlayerTarget lookup fallback (exact, then +s, then -s) so a
// wiki task name that's merely the singular/plural of an already-covered key isn't reported.
function isKnown(taskName, knownNames) {
  const name = taskName.toLowerCase();
  if (knownNames.has(name)) return true;
  if (knownNames.has(`${name}s`)) return true;
  if (name.endsWith("s") && knownNames.has(name.slice(0, -1))) return true;
  return false;
}

async function main() {
  const knownNames = await getKnownTaskNames();
  const monsterPages = await fetchHasTaskPages();

  const taskNameToMonsters = new Map();
  for (let i = 0; i < monsterPages.length; i += BATCH_SIZE) {
    const batch = monsterPages.slice(i, i + BATCH_SIZE).map((p) => p.title);
    const results = await fetchTaskNamesForBatch(batch);
    for (const { monster, taskName } of results) {
      if (!taskNameToMonsters.has(taskName)) taskNameToMonsters.set(taskName, []);
      taskNameToMonsters.get(taskName).push(monster);
    }
  }

  const candidates = [...taskNameToMonsters.entries()]
    .filter(([taskName]) => !isKnown(taskName, knownNames))
    .map(([taskName, monsters]) => ({
      taskName,
      monsters,
      wikiUrl: `https://oldschool.runescape.wiki/w/${encodeURIComponent(taskName.replace(/ /g, "_"))}`,
    }));

  await fs.writeFile(REPORT_PATH, JSON.stringify(candidates, null, 2));
  console.log(JSON.stringify(candidates, null, 2));
  console.log(
    `\n${candidates.length} candidate slayer task name(s) not in SLAYER_MONSTER_ICONS (out of ${taskNameToMonsters.size} distinct task names found across ${monsterPages.length} monster pages).`
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
