// Reports OSRS Wiki achievement diary areas/tiers whose task count no longer matches
// diary_data.json, for manual triage. Read-only: never touches diary_data.json.
//
// Diary tasks don't have their own per-task wiki page/infobox (unlike quests/CA tasks), so this
// can't reuse the embeddedin-infobox pattern the sibling detect-new-quests.mjs/
// detect-new-ca-tasks.mjs use. Instead it fetches each area's "<Area> Diary" wikitext page and
// counts the numbered task rows in each tier's `data-diary-tier="<Tier>"` table (each row starts
// "|N. <task text>" right after a "|-" row separator), then diffs that count against
// diary_data.json[area][tier].length. A count mismatch means a task was added/removed/split on
// the wiki - it does NOT catch pure task-text rewording (e.g. a boss rename) with no count
// change, so still worth a periodic manual reread even with a clean report.
import axios from "axios";
import fs from "fs/promises";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SITE_ROOT = path.resolve(__dirname, "..", "..");
const DIARY_DATA_PATH = path.join(SITE_ROOT, "public", "data", "diary_data.json");
const REPORT_PATH = path.join(__dirname, "new-diary-tasks-report.json");

const USER_AGENT = "GroupScape-DataPipeline/1.0 (contact: repo issue tracker)";
const WIKI_API = "https://oldschool.runescape.wiki/api.php";

// diary_data.json's area keys double as the wiki page title prefix ("<Area> Diary") for every
// area - verified against the wiki's opensearch for each one.
const TIERS = ["Easy", "Medium", "Hard", "Elite"];

async function getKnownDiaryData() {
  const raw = await fs.readFile(DIARY_DATA_PATH, "utf-8");
  return JSON.parse(raw);
}

async function fetchAreaWikitext(area) {
  const res = await axios.get(WIKI_API, {
    headers: { "User-Agent": USER_AGENT },
    params: {
      action: "parse",
      page: `${area} Diary`,
      prop: "wikitext",
      format: "json",
    },
  });
  const wikitext = res.data?.parse?.wikitext?.["*"];
  if (typeof wikitext !== "string") {
    throw new Error(`Unexpected parse response for "${area} Diary": ${JSON.stringify(res.data).slice(0, 200)}`);
  }
  return wikitext;
}

function countTierTasks(wikitext, tier) {
  const marker = `data-diary-tier="${tier}"`;
  const start = wikitext.indexOf(marker);
  if (start === -1) return null;
  const nextMarker = wikitext.indexOf('data-diary-tier="', start + marker.length);
  const end = nextMarker === -1 ? wikitext.indexOf("===Rewards===", start) : nextMarker;
  const block = wikitext.slice(start, end === -1 ? undefined : end);
  const matches = block.match(/\|-\r?\n\| ?\d+\.\s/g);
  return matches ? matches.length : 0;
}

async function main() {
  const knownData = await getKnownDiaryData();
  const areas = Object.keys(knownData);
  const mismatches = [];

  for (const area of areas) {
    const wikitext = await fetchAreaWikitext(area);
    for (const tier of TIERS) {
      const wikiCount = countTierTasks(wikitext, tier);
      const knownCount = knownData[area]?.[tier]?.length ?? 0;
      if (wikiCount === null) {
        mismatches.push({ area, tier, issue: "tier table not found on wiki page", knownCount });
        continue;
      }
      if (wikiCount !== knownCount) {
        mismatches.push({
          area,
          tier,
          issue: "task count mismatch",
          wikiCount,
          knownCount,
          wikiUrl: `https://oldschool.runescape.wiki/w/${encodeURIComponent(`${area} Diary`.replace(/ /g, "_"))}`,
        });
      }
    }
  }

  await fs.writeFile(REPORT_PATH, JSON.stringify(mismatches, null, 2));
  console.log(JSON.stringify(mismatches, null, 2));
  console.log(
    `\n${mismatches.length} area/tier mismatch(es) out of ${areas.length * TIERS.length} checked. ` +
      `Note: this only catches count changes, not reworded tasks with the same count - worth a periodic manual reread regardless.`
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
