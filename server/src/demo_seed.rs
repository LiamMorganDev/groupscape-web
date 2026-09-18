//! Seeds (and, run again, refreshes) the public "@EXAMPLE" demo group with realistic-looking
//! data across every table the real app reads from - not just the couple of fields the old
//! frontend-only mock (`example-data.js`, now removed) covered.
//!
//! Called from two places: the `seed` binary (`cargo run --bin seed [--include-admin-data]`,
//! also what the production demo-reset sidecar runs on a loop - see `docker-compose.prod.yml`),
//! and optionally `main.rs` itself at startup when `AUTO_SEED_DEMO_DATA` is set, so a fresh local
//! dev DB gets a working demo group without a separate manual step.
use crate::crypto;
use crate::db;
use crate::demo::DEMO_GROUP_NAME;
use crate::drop_rates::slugify_npc_name;
use crate::error::ApiError;
use crate::models::{
    CreateGroup, DeathEvent, GameEvent, KillEvent, LootEvent, LootItem, LootSourceType,
};
use chrono::{DateTime, Duration, Utc};
use deadpool_postgres::Client;
use rand_core::{OsRng, RngCore};

/// The zero-UUID token `demo-page.js` has always stored for "@EXAMPLE" - public and non-secret
/// by design, which is exactly why every write handler now rejects it (see `crate::demo`).
const DEMO_GROUP_TOKEN: &str = "00000000-0000-0000-0000-000000000000";
const DEMO_MEMBERS: [&str; 5] = ["Bronze Boots", "Iron Wolf", "Steel Fox", "Mithril Owl", "Addy Hawk"];
const HISTORY_DAYS: i64 = 21;
/// Username for the sole account that owns the demo members' fabricated `characters` rows,
/// purely to satisfy `characters.account_id`'s NOT NULL FK - not a login anyone can use (random
/// password, never printed), and seeded regardless of `--include-admin-data` since portraits are
/// part of the public demo, not the dev-only admin-panel data.
const PORTRAIT_OWNER_USERNAME: &str = "example-portraits-system";

fn rand_below(bound: u32) -> u32 {
    if bound == 0 {
        0
    } else {
        OsRng.next_u32() % bound
    }
}

fn rand_range(min: i64, max_inclusive: i64) -> i64 {
    min + rand_below((max_inclusive - min + 1) as u32) as i64
}

fn choose<T>(items: &[T]) -> &T {
    &items[rand_below(items.len() as u32) as usize]
}

/// Runs the full seed pass: ensure the demo group and its members exist, refresh their baseline
/// stats, regenerate the last `HISTORY_DAYS` of activity/bank history, ensure portraits, and -
/// only when `include_admin_data` is true - seed dev/QA-only admin-panel data.
pub async fn run(client: &mut Client, include_admin_data: bool) -> Result<i64, ApiError> {
    let group_id = ensure_demo_group(client).await?;
    seed_member_baselines(client, group_id).await?;
    regenerate_history(client, group_id).await?;
    seed_portraits(client, group_id).await?;
    if include_admin_data {
        seed_admin_data(client, group_id).await?;
    }
    Ok(group_id)
}

/// Entities are upserted, not wiped: the demo group/members are created once and then matched by
/// name on every later run, so `member_id`/`account_hash`/portrait linkage stay stable across
/// resets instead of being torn down and rebuilt.
async fn ensure_demo_group(client: &mut Client) -> Result<i64, ApiError> {
    if let Some(group_id) = db::get_group_id_by_name(client, DEMO_GROUP_NAME).await? {
        let existing_stmt = client
            .prepare_cached("SELECT member_name FROM groupscape.members WHERE group_id=$1")
            .await?;
        let existing: std::collections::HashSet<String> = client
            .query(&existing_stmt, &[&group_id])
            .await?
            .iter()
            .map(|row| row.get::<_, String>(0))
            .collect();
        for member_name in DEMO_MEMBERS {
            if !existing.contains(member_name) {
                db::add_group_member(client, group_id, member_name).await?;
            }
        }
        return Ok(group_id);
    }

    let create_group = CreateGroup {
        name: DEMO_GROUP_NAME.to_string(),
        member_names: DEMO_MEMBERS.iter().map(|name| name.to_string()).collect(),
        captcha_response: String::new(),
        token: DEMO_GROUP_TOKEN.to_string(),
    };
    db::create_group(client, &create_group).await?;
    db::get_group_id_by_name(client, DEMO_GROUP_NAME)
        .await?
        .ok_or_else(|| ApiError::SeedError("group_id missing immediately after create_group".to_string()))
}

/// Fills in each demo member's "current" columns (what player-stats/skills/inventory/equipment
/// read) with a plausible, distinct loadout per member - direct SQL rather than
/// `db::update_group_member`, since that path is shaped for one real plugin heartbeat at a time,
/// not a batch seed of five members' full state at once.
async fn seed_member_baselines(client: &Client, group_id: i64) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
UPDATE groupscape.members SET
  stats=$3, stats_last_update=now(),
  coordinates=$4, coordinates_last_update=now(),
  skills=$5, skills_last_update=now(),
  inventory=$6, inventory_last_update=now(),
  equipment=$7, equipment_last_update=now(),
  rune_pouch=$8, rune_pouch_last_update=now(),
  bank=$9, bank_last_update=now()
WHERE group_id=$1 AND member_name=$2
"#,
        )
        .await?;

    // Common, well-known item ids so the inventory/equipment/bank panels show recognizable gear
    // rather than arbitrary numbers - coins(995), shark(385), dragon bones(536), whip(4151),
    // dragon defender(12954), fire cape(6570).
    for (index, member_name) in DEMO_MEMBERS.iter().enumerate() {
        let stats = vec![99, 99, 70 - index as i32 * 5, 99, 100, 100, 420];
        let skills: Vec<i32> = (0..24).map(|_| rand_range(50_000, 13_000_000) as i32).collect();
        let coordinates = vec![3200 + index as i32 * 5, 3200, 0];
        let inventory = vec![995, 250_000 + index as i32 * 10_000, 385, 8, -1, 0, -1, 0];
        let mut inventory_full = inventory.clone();
        inventory_full.resize(56, -1);
        for pair in inventory_full.chunks_mut(2).skip(inventory.len() / 2) {
            pair[0] = -1;
            pair[1] = 0;
        }
        let mut equipment = vec![-1; 28];
        equipment[0] = 4151; // weapon: abyssal whip
        equipment[1] = 1;
        equipment[2] = 12954; // shield slot: dragon defender
        equipment[3] = 1;
        let rune_pouch = vec![561, 1000, 555, 1000, 562, 500, -1, 0];
        let bank = vec![995, 5_000_000 + index as i32 * 250_000, 536, 500, 6570, 1];

        client
            .execute(
                &stmt,
                &[
                    &group_id,
                    member_name,
                    &stats,
                    &coordinates,
                    &skills,
                    &inventory_full,
                    &equipment,
                    &rune_pouch,
                    &bank,
                ],
            )
            .await?;
    }
    Ok(())
}

/// Time-series tables have no stable per-row identity to upsert against, so they're wiped for
/// this group and regenerated fresh every run, with timestamps computed relative to "now" - the
/// demo always looks like "the last few weeks" no matter when the scheduled reset last ran.
async fn regenerate_history(client: &Client, group_id: i64) -> Result<(), ApiError> {
    client
        .execute(
            "DELETE FROM groupscape.activity_events WHERE group_id=$1",
            &[&group_id],
        )
        .await?;
    client
        .execute(
            "DELETE FROM groupscape.bank_value_snapshots WHERE member_id IN (SELECT member_id FROM groupscape.members WHERE group_id=$1)",
            &[&group_id],
        )
        .await?;

    let session_id = db::ensure_open_session(client, group_id).await?;
    let bosses = ["Vorkath", "Zulrah", "Cerberus", "General Graardor", "Giant Mole"];
    let chests = ["Chambers of Xeric", "Theatre of Blood", "Barrows", "Tombs of Amascut"];
    let clue_tiers = ["beginner", "easy", "medium", "hard", "elite", "master"];
    let now = Utc::now();

    for day_offset in (0..HISTORY_DAYS).rev() {
        let day = now - Duration::days(day_offset);
        for member_name in DEMO_MEMBERS {
            // 0-3 kills for this member on this day.
            for _ in 0..rand_range(0, 3) {
                let occurred_at = day - Duration::minutes(rand_range(0, 1439));
                let boss = choose(&bosses);
                let event = GameEvent::Kill(KillEvent {
                    npc_id: rand_range(1, 20000) as i32,
                    npc_name: boss.to_string(),
                    world_x: 3200 + rand_range(-50, 50) as i32,
                    world_y: 3200 + rand_range(-50, 50) as i32,
                    plane: 0,
                    world: 420,
                    occurred_at: Some(occurred_at),
                    participants: Some(vec![member_name.to_string()]),
                    loot: Some(vec![LootItem {
                        item_id: 536,
                        quantity: rand_range(1, 4) as i32,
                    }]),
                    account_kc: None,
                    event_id: None,
                    sub_kills: None,
                    delve_level: None,
                });
                insert_backdated_event(client, group_id, session_id, member_name, &event, occurred_at)
                    .await?;
            }
            // 0-1 chest openings for this member on this day.
            if rand_below(3) == 0 {
                let occurred_at = day - Duration::minutes(rand_range(0, 1439));
                let event = GameEvent::Loot(LootEvent {
                    source_type: LootSourceType::Chest,
                    source_name: choose(&chests).to_string(),
                    clue_tier: None,
                    world_x: 3200,
                    world_y: 3200,
                    plane: 0,
                    world: 420,
                    occurred_at: Some(occurred_at),
                    loot: vec![LootItem {
                        item_id: 536,
                        quantity: rand_range(1, 4) as i32,
                    }],
                    event_id: None,
                });
                insert_backdated_event(client, group_id, session_id, member_name, &event, occurred_at)
                    .await?;
            }
            // An occasional clue scroll casket, rarer still.
            if rand_below(6) == 0 {
                let occurred_at = day - Duration::minutes(rand_range(0, 1439));
                let tier = choose(&clue_tiers);
                let event = GameEvent::Loot(LootEvent {
                    source_type: LootSourceType::Clue,
                    source_name: format!("Clue Scroll ({})", tier),
                    clue_tier: Some(tier.to_string()),
                    world_x: 3200,
                    world_y: 3200,
                    plane: 0,
                    world: 420,
                    occurred_at: Some(occurred_at),
                    loot: vec![LootItem {
                        item_id: 536,
                        quantity: rand_range(1, 4) as i32,
                    }],
                    event_id: None,
                });
                insert_backdated_event(client, group_id, session_id, member_name, &event, occurred_at)
                    .await?;
            }
            // An occasional death, much rarer than kills.
            if rand_below(20) == 0 {
                let occurred_at = day - Duration::minutes(rand_range(0, 1439));
                let event = GameEvent::Death(DeathEvent {
                    world_x: 3200,
                    world_y: 3200,
                    plane: 0,
                    world: 420,
                    occurred_at: Some(occurred_at),
                    killer_name: Some(choose(&bosses).to_string()),
                    doom_delve_level: None,
                    event_id: None,
                });
                insert_backdated_event(client, group_id, session_id, member_name, &event, occurred_at)
                    .await?;
            }
        }
    }

    // Bank value trending gently upward over the window, per member, one snapshot per day.
    let member_ids_stmt = client
        .prepare_cached("SELECT member_id FROM groupscape.members WHERE group_id=$1 AND member_name != '@SHARED'")
        .await?;
    let member_ids: Vec<i64> = client
        .query(&member_ids_stmt, &[&group_id])
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let snapshot_stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.bank_value_snapshots (member_id, snapshot_date, captured_at, bank_value) VALUES ($1, $2, $3, $4) ON CONFLICT (member_id, snapshot_date) DO UPDATE SET captured_at=excluded.captured_at, bank_value=excluded.bank_value",
        )
        .await?;
    for member_id in member_ids {
        let base = rand_range(3_000_000, 8_000_000);
        for day_offset in (0..HISTORY_DAYS).rev() {
            let day = now - Duration::days(day_offset);
            let drift = (HISTORY_DAYS - day_offset) * rand_range(20_000, 80_000);
            let value = base + drift;
            client
                .execute(&snapshot_stmt, &[&member_id, &day.date_naive(), &day, &value])
                .await?;
        }
    }

    Ok(())
}

async fn insert_backdated_event(
    client: &Client,
    group_id: i64,
    session_id: i64,
    member_name: &str,
    event: &GameEvent,
    occurred_at: DateTime<Utc>,
) -> Result<(), ApiError> {
    let payload = serde_json::to_value(event)?;
    let npc_slug = (event.event_type() == "kill")
        .then(|| {
            payload
                .get("npcName")
                .and_then(|v| v.as_str())
                .map(slugify_npc_name)
        })
        .flatten();
    client
        .execute(
            "INSERT INTO groupscape.activity_events (session_id, group_id, member_name, event_type, occurred_at, payload, npc_slug) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &session_id,
                &group_id,
                &member_name,
                &event.event_type(),
                &occurred_at,
                &payload,
                &npc_slug,
            ],
        )
        .await?;
    Ok(())
}

/// A minimal, valid ASCII PLY mesh (a small flat-shaded cube) - three.js's `PLYLoader` accepts
/// ASCII PLY, not just the binary format the real plugin uploads, so this renders as an actual
/// (if crude) 3D placeholder in the portrait viewer rather than erroring - used only when no real
/// `character_mesh` row is available to copy from (i.e. a fresh local DB with no real users yet).
fn placeholder_ply() -> Vec<u8> {
    const PLY: &str = r#"ply
format ascii 1.0
element vertex 8
property float x
property float y
property float z
property uchar red
property uchar green
property uchar blue
element face 6
property list uchar int vertex_indices
end_header
-1 -1 -1 120 90 60
1 -1 -1 120 90 60
1 1 -1 120 90 60
-1 1 -1 120 90 60
-1 -1 1 150 110 70
1 -1 1 150 110 70
1 1 1 150 110 70
-1 1 1 150 110 70
4 0 1 2 3
4 4 5 6 7
4 0 1 5 4
4 2 3 7 6
4 0 3 7 4
4 1 2 6 5
"#;
    PLY.as_bytes().to_vec()
}

/// Ensures the demo members have a browsable 3D portrait: a hidden account+character row per
/// member (purely so `characters.account_id`'s FK is satisfied - never a real login) holding a
/// mesh copied from a random real user's `character_mesh`, or the placeholder above when none
/// exist yet (fresh local DB). `members.account_hash` is set to match, since that's how
/// `db::get_member_mesh` joins portraits to the group panel.
async fn seed_portraits(client: &mut Client, group_id: i64) -> Result<(), ApiError> {
    let owner_account_id = match db::get_account_by_username(client, PORTRAIT_OWNER_USERNAME).await? {
        Some(account) => account.id,
        None => {
            let password_hash = crypto::hash_password(&crypto::generate_temp_password())
                .map_err(|e| ApiError::SeedError(e.to_string()))?;
            db::create_account(client, PORTRAIT_OWNER_USERNAME, &password_hash).await?
        }
    };

    // Real meshes to borrow from, excluding this owner account's own (fabricated) characters.
    let real_meshes: Vec<Vec<u8>> = client
        .query(
            "SELECT cm.mesh FROM groupscape.character_mesh cm INNER JOIN groupscape.characters c ON c.character_id=cm.character_id WHERE c.account_id != $1 ORDER BY random() LIMIT $2",
            &[&owner_account_id, &(DEMO_MEMBERS.len() as i64)],
        )
        .await?
        .iter()
        .map(|row| row.get::<_, Vec<u8>>(0))
        .collect();

    for (index, member_name) in DEMO_MEMBERS.iter().enumerate() {
        let account_hash = format!("demo-portrait-{}", member_name.to_lowercase().replace(' ', "-"));
        let character_id = match db::find_character_by_account_hash(client, &account_hash).await? {
            Some(character) => character.id,
            None => {
                db::create_character(client, owner_account_id, &account_hash, member_name)
                    .await?
                    .id
            }
        };

        client
            .execute(
                "UPDATE groupscape.members SET account_hash=$1 WHERE group_id=$2 AND member_name=$3",
                &[&account_hash, &group_id, member_name],
            )
            .await?;

        let mesh = real_meshes.get(index).cloned().unwrap_or_else(placeholder_ply);
        db::upsert_character_mesh(client, character_id, &mesh).await?;
    }

    Ok(())
}

/// Dev/QA-only: gives the admin panel something to show locally. Never run in production - the
/// production reset sidecar never passes `--include-admin-data` (see docker-compose.prod.yml),
/// so no demo account/session credentials ever exist in a production database.
async fn seed_admin_data(client: &mut Client, group_id: i64) -> Result<(), ApiError> {
    for (index, member_name) in DEMO_MEMBERS.iter().enumerate() {
        let username = format!("demo-admin-{}", index);
        let account_id = match db::get_account_by_username(client, &username).await? {
            Some(account) => account.id,
            None => {
                let password_hash = crypto::hash_password(&crypto::generate_temp_password())
                    .map_err(|e| ApiError::SeedError(e.to_string()))?;
                db::create_account(client, &username, &password_hash).await?
            }
        };
        let account_hash = format!("demo-admin-hash-{}", index);
        let character_id = match db::find_character_by_account_hash(client, &account_hash).await? {
            Some(character) => character.id,
            None => {
                db::create_character(client, account_id, &account_hash, member_name)
                    .await?
                    .id
            }
        };
        // Idempotent: link_character_to_group is a no-op if already linked to this same group.
        let _ = db::link_character_to_group(client, character_id, account_id, group_id).await;
    }

    db::admin_record_audit_log(
        client,
        "seed_demo_data",
        Some("group"),
        Some(&group_id.to_string()),
        Some(serde_json::json!({ "note": "Demo admin data seeded for local dev/QA" })),
    )
    .await?;

    Ok(())
}
