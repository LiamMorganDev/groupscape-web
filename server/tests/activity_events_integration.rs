use chrono::{Duration, Utc};
use deadpool_postgres::{ManagerConfig, Pool, RecyclingMethod};
use std::env;
use tokio_postgres::NoTls;

use server::config::Config;
use server::db;
use server::leaderboard::LeaderboardWindow;
use server::models::{DeathEvent, DiscordWebhookSettings, GameEvent, KillEvent, LootItem};
use server::progress_events::ProgressEvent;

/// Serializes integration tests since they all share the same database
/// and each test drops/recreates the schema.
static TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn create_test_pool() -> Pool {
    let mut cfg = if let Ok(url) = env::var("TEST_DATABASE_URL") {
        let mut c = deadpool_postgres::Config::new();
        c.url = Some(url);
        c
    } else {
        let config = Config::from_env().expect("failed to read config.toml");
        let mut pg = config.pg.clone();
        pg.dbname = Some("groupscape_test".to_string());
        pg
    };

    cfg.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    });

    cfg.create_pool(None, NoTls)
        .expect("failed to create test pool")
}

/// Fresh schema, migrated by the server's own `db::update_schema`.
async fn setup(pool: &Pool) {
    let mut client = pool.get().await.expect("failed to get client");
    client
        .execute("DROP SCHEMA IF EXISTS groupscape CASCADE", &[])
        .await
        .expect("failed to drop schema");
    client
        .execute("CREATE SCHEMA IF NOT EXISTS groupscape", &[])
        .await
        .expect("failed to create schema");
    client
        .execute(
            r#"CREATE TABLE groupscape.groups(
                group_id BIGSERIAL UNIQUE,
                group_name TEXT NOT NULL,
                group_token_hash CHAR(64) NOT NULL,
                PRIMARY KEY (group_name, group_token_hash)
            )"#,
            &[],
        )
        .await
        .expect("failed to create groups table");

    db::update_schema(&mut client)
        .await
        .expect("failed to update schema");
}

async fn create_test_group(client: &deadpool_postgres::Client, name: &str) -> i64 {
    let hashed_token = server::crypto::token_hash("test-token", name);
    let row = client
        .query_one(
            "INSERT INTO groupscape.groups (group_name, group_token_hash, version) VALUES ($1, $2, 2) RETURNING group_id",
            &[&name, &hashed_token],
        )
        .await
        .expect("failed to create test group");
    row.get(0)
}

fn sample_kill() -> GameEvent {
    GameEvent::Kill(KillEvent {
        npc_id: 3129,
        npc_name: "Zulrah".to_string(),
        world_x: 2200,
        world_y: 3057,
        plane: 0,
        world: 420,
        occurred_at: None,
        participants: None,
        loot: Some(vec![LootItem {
            item_id: 12934,
            quantity: 1,
        }]),
        account_kc: None,
        event_id: None,
        sub_kills: None,
        delve_level: None,
    })
}

fn sample_death() -> GameEvent {
    GameEvent::Death(DeathEvent {
        world_x: 3200,
        world_y: 3200,
        plane: 0,
        world: 420,
        occurred_at: None,
        killer_name: Some("Zulrah".to_string()),
        doom_delve_level: None,
        event_id: None,
    })
}

#[tokio::test]
async fn test_ensure_open_session_is_idempotent_and_atomic() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "sessiontest1").await;

    let first = db::ensure_open_session(&client, group_id)
        .await
        .expect("first ensure should succeed");
    let second = db::ensure_open_session(&client, group_id)
        .await
        .expect("second ensure should succeed");
    assert_eq!(
        first, second,
        "heartbeats within the same window reuse the open session"
    );

    let sessions = db::list_sessions(&client, group_id, 10)
        .await
        .expect("query failed");
    assert_eq!(
        sessions.len(),
        1,
        "only one open session should exist per group"
    );
    assert!(sessions[0].ended_at.is_none());
}

#[tokio::test]
async fn test_ensure_open_session_scopes_per_group() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_a = create_test_group(&client, "sessiontest2a").await;
    let group_b = create_test_group(&client, "sessiontest2b").await;

    let session_a = db::ensure_open_session(&client, group_a).await.unwrap();
    let session_b = db::ensure_open_session(&client, group_b).await.unwrap();
    assert_ne!(session_a, session_b);
}

#[tokio::test]
async fn test_close_idle_sessions_closes_only_past_the_cutoff() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let fresh_group = create_test_group(&client, "sessiontest3fresh").await;
    let stale_group = create_test_group(&client, "sessiontest3stale").await;

    db::ensure_open_session(&client, fresh_group).await.unwrap();
    let stale_session = db::ensure_open_session(&client, stale_group).await.unwrap();
    client
        .execute(
            "UPDATE groupscape.sessions SET last_seen_at = $1 WHERE session_id = $2",
            &[&(Utc::now() - Duration::minutes(20)), &stale_session],
        )
        .await
        .unwrap();

    let closed = db::close_idle_sessions(&client, Duration::minutes(15))
        .await
        .expect("close should succeed");
    assert_eq!(closed, 1);

    let fresh_sessions = db::list_sessions(&client, fresh_group, 10).await.unwrap();
    assert!(
        fresh_sessions[0].ended_at.is_none(),
        "fresh session should stay open"
    );

    let stale_sessions = db::list_sessions(&client, stale_group, 10).await.unwrap();
    assert!(
        stale_sessions[0].ended_at.is_some(),
        "stale session should be closed"
    );

    // A heartbeat after close opens a brand new session rather than reusing the closed one.
    let reopened = db::ensure_open_session(&client, stale_group).await.unwrap();
    assert_ne!(reopened, stale_session);
}

#[tokio::test]
async fn test_insert_and_list_activity_events() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest1").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    db::insert_activity_event(&client, group_id, session_id, "Zezima", &sample_kill())
        .await
        .expect("insert should succeed");
    db::insert_activity_event(&client, group_id, session_id, "Zezima", &sample_death())
        .await
        .expect("insert should succeed");

    let events = db::list_activity_events(&client, group_id, None, None, None, 30)
        .await
        .expect("query failed");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "death", "newest event sorts first");
    assert_eq!(events[1].event_type, "kill");
    assert_eq!(events[0].member_name, "Zezima");
    assert_eq!(events[0].session_id, session_id);
    assert_eq!(
        events[1].payload["npcName"],
        serde_json::json!("Zulrah"),
        "payload should round-trip the plugin's exact field names"
    );
}

#[tokio::test]
async fn test_list_activity_events_filters_by_member_and_type() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest2").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    db::insert_activity_event(&client, group_id, session_id, "Zezima", &sample_kill())
        .await
        .unwrap();
    db::insert_activity_event(&client, group_id, session_id, "Woox", &sample_death())
        .await
        .unwrap();

    let zezima_events = db::list_activity_events(&client, group_id, Some("Zezima"), None, None, 30)
        .await
        .unwrap();
    assert_eq!(zezima_events.len(), 1);
    assert_eq!(zezima_events[0].member_name, "Zezima");

    let kill_events = db::list_activity_events(&client, group_id, None, Some("kill"), None, 30)
        .await
        .unwrap();
    assert_eq!(kill_events.len(), 1);
    assert_eq!(kill_events[0].event_type, "kill");
}

#[tokio::test]
async fn test_list_activity_events_is_scoped_per_group() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_a = create_test_group(&client, "activitytest3a").await;
    let group_b = create_test_group(&client, "activitytest3b").await;
    let session_a = db::ensure_open_session(&client, group_a).await.unwrap();

    db::insert_activity_event(&client, group_a, session_a, "Zezima", &sample_kill())
        .await
        .unwrap();

    let group_b_events = db::list_activity_events(&client, group_b, None, None, None, 30)
        .await
        .unwrap();
    assert!(group_b_events.is_empty());
}

#[tokio::test]
async fn test_kill_event_without_loot_round_trips() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest4").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    let kill_without_loot = GameEvent::Kill(KillEvent {
        npc_id: 50,
        npc_name: "Vorkath".to_string(),
        world_x: 0,
        world_y: 0,
        plane: 0,
        world: 301,
        occurred_at: None,
        participants: None,
        loot: None,
        account_kc: None,
        event_id: None,
        sub_kills: None,
        delve_level: None,
    });
    db::insert_activity_event(&client, group_id, session_id, "Zezima", &kill_without_loot)
        .await
        .expect("insert should succeed even when the plugin never correlated loot");

    let events = db::list_activity_events(&client, group_id, None, None, None, 30)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].payload.get("loot").is_none());
}

/// Regression test for kills/loot/deaths getting duplicated on server restart: the plugin
/// resends a buffered event verbatim (same `event_id`) when it can't tell a dropped connection
/// from an outright failure - `insert_activity_event` must reject the replay rather than writing
/// a second row, and report that back so the caller doesn't fire a second Discord message either.
#[tokio::test]
async fn test_insert_activity_event_is_idempotent_on_matching_event_id() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest8").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    let kill = GameEvent::Kill(KillEvent {
        npc_id: 3129,
        npc_name: "Zulrah".to_string(),
        world_x: 2200,
        world_y: 3057,
        plane: 0,
        world: 420,
        occurred_at: None,
        participants: None,
        loot: None,
        account_kc: None,
        event_id: Some("replayed-kill-1".to_string()),
        sub_kills: None,
        delve_level: None,
    });

    let first_insert = db::insert_activity_event(&client, group_id, session_id, "Zezima", &kill)
        .await
        .expect("first insert should succeed");
    assert!(first_insert, "first send of an event should be inserted");

    // Simulates the plugin resending the same buffered event after a connection drop whose
    // response never reached it (e.g. the server restarting mid-request).
    let second_insert = db::insert_activity_event(&client, group_id, session_id, "Zezima", &kill)
        .await
        .expect("replayed insert should not error");
    assert!(
        !second_insert,
        "a resend with the same event_id must be rejected as a duplicate"
    );

    let events = db::list_activity_events(&client, group_id, None, None, None, 30)
        .await
        .unwrap();
    assert_eq!(events.len(), 1, "the replay must not create a second row");
}

#[tokio::test]
async fn test_list_activity_events_hides_non_notable_kills_but_keeps_all_deaths() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest7").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    let ordinary_kill = GameEvent::Kill(KillEvent {
        npc_id: 41,
        npc_name: "Cow".to_string(),
        world_x: 0,
        world_y: 0,
        plane: 0,
        world: 301,
        occurred_at: None,
        participants: None,
        loot: None,
        account_kc: None,
        event_id: None,
        sub_kills: None,
        delve_level: None,
    });
    let boss_kill = sample_kill();
    let death_by_ordinary_npc = GameEvent::Death(DeathEvent {
        world_x: 0,
        world_y: 0,
        plane: 0,
        world: 301,
        occurred_at: None,
        killer_name: Some("Cow".to_string()),
        doom_delve_level: None,
        event_id: None,
    });

    db::insert_activity_event(&client, group_id, session_id, "Zezima", &ordinary_kill)
        .await
        .unwrap();
    db::insert_activity_event(&client, group_id, session_id, "Zezima", &boss_kill)
        .await
        .unwrap();
    db::insert_activity_event(
        &client,
        group_id,
        session_id,
        "Zezima",
        &death_by_ordinary_npc,
    )
    .await
    .unwrap();

    let events = db::list_activity_events(&client, group_id, None, None, None, 30)
        .await
        .unwrap();
    assert_eq!(
        events.len(),
        2,
        "the ordinary-NPC kill should be hidden but the boss kill and the death should stay"
    );
    assert!(events.iter().any(|e| e.event_type == "death"));
    assert!(events
        .iter()
        .any(|e| e.event_type == "kill" && e.payload["npcName"] == serde_json::json!("Zulrah")));
}

#[tokio::test]
async fn test_progress_events_round_trip() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest5").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    let milestones = [
        ProgressEvent {
            event_type: "quest",
            payload: serde_json::json!({ "quest_id": 12 }),
        },
        ProgressEvent {
            event_type: "diary",
            payload: serde_json::json!({ "region": "Kandarin", "tier": "Elite" }),
        },
        ProgressEvent {
            event_type: "combat_task",
            payload: serde_json::json!({ "task_id": 300 }),
        },
        ProgressEvent {
            event_type: "collection_log",
            payload: serde_json::json!({ "kind": "page", "page": "Abyssal Sire" }),
        },
    ];
    for event in &milestones {
        db::insert_progress_event(&client, group_id, session_id, "Zezima", event)
            .await
            .expect("insert should succeed");
    }

    let events = db::list_activity_events(&client, group_id, None, None, None, 30)
        .await
        .unwrap();
    assert_eq!(events.len(), 4);
    assert!(events.iter().all(|e| e.member_name == "Zezima"));
    assert!(events
        .iter()
        .any(|e| e.event_type == "diary" && e.payload["region"] == serde_json::json!("Kandarin")));
    assert!(events.iter().any(
        |e| e.event_type == "collection_log" && e.payload["kind"] == serde_json::json!("page")
    ));
}

/// Rows written before the feed was scoped down to milestones (NPC dialogue, object
/// interactions) must stop surfacing, not just stop being written.
#[tokio::test]
async fn test_out_of_scope_event_types_are_excluded_from_the_feed() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "activitytest6").await;
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    for event_type in ["dialogue", "object_interaction"] {
        db::insert_activity_event_payload(
            &client,
            group_id,
            session_id,
            "Zezima",
            event_type,
            serde_json::json!({ "npcName": "Hans" }),
            None,
            None,
        )
        .await
        .expect("insert should succeed");
    }
    db::insert_activity_event(&client, group_id, session_id, "Zezima", &sample_kill())
        .await
        .unwrap();

    let events = db::list_activity_events(&client, group_id, None, None, None, 30)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "kill");
}

#[tokio::test]
async fn test_discord_webhook_settings_default_to_disabled_and_all_notify_true() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "discordtest1").await;

    let settings = db::get_discord_webhook_settings(&client, group_id)
        .await
        .expect("query should succeed");
    assert_eq!(settings.webhook_url, None);
    assert!(settings.notify_kills);
    assert!(settings.notify_deaths);
    assert!(settings.notify_drops);
    assert_eq!(settings.drops_min_value, 250000);
    assert!(!settings.drops_unique_only);
    assert!(settings.notify_combat_achievements);
    assert!(settings.notify_collection_log);
    assert!(settings.notify_quests);
    assert!(settings.notify_diaries);
    assert!(settings.notify_pets);
    assert!(settings.notify_clues);
    assert!(settings.notify_level_ups);
    assert_eq!(settings.level_up_interval, 10);
}

#[tokio::test]
async fn test_discord_webhook_settings_round_trip() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "discordtest2").await;

    let updated = DiscordWebhookSettings {
        webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
        notify_kills: true,
        notify_deaths: false,
        notify_drops: true,
        drops_min_value: 500000,
        drops_unique_only: false,
        notify_raids: true,
        notify_combat_achievements: true,
        notify_collection_log: true,
        notify_quests: true,
        notify_diaries: true,
        notify_pets: true,
        notify_clues: true,
        notify_level_ups: true,
        level_up_interval: 10,
    };
    db::update_discord_webhook_settings(&client, group_id, &updated)
        .await
        .expect("update should succeed");

    let settings = db::get_discord_webhook_settings(&client, group_id)
        .await
        .expect("query should succeed");
    assert_eq!(settings.webhook_url, updated.webhook_url);
    assert!(settings.notify_kills);
    assert!(!settings.notify_deaths);
    assert!(settings.notify_drops);
    assert_eq!(settings.drops_min_value, 500000);
}

#[tokio::test]
async fn test_discord_webhook_settings_url_can_be_cleared() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "discordtest3").await;

    db::update_discord_webhook_settings(
        &client,
        group_id,
        &DiscordWebhookSettings {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            notify_kills: true,
            notify_deaths: true,
            notify_drops: true,
            drops_min_value: 250000,
            drops_unique_only: false,
            notify_raids: true,
            notify_combat_achievements: true,
            notify_collection_log: true,
            notify_quests: true,
            notify_diaries: true,
            notify_pets: true,
            notify_clues: true,
            notify_level_ups: true,
            level_up_interval: 10,
        },
    )
    .await
    .unwrap();

    db::update_discord_webhook_settings(
        &client,
        group_id,
        &DiscordWebhookSettings {
            webhook_url: None,
            notify_kills: true,
            notify_deaths: true,
            notify_drops: true,
            drops_min_value: 250000,
            drops_unique_only: false,
            notify_raids: true,
            notify_combat_achievements: true,
            notify_collection_log: true,
            notify_quests: true,
            notify_diaries: true,
            notify_pets: true,
            notify_clues: true,
            notify_level_ups: true,
            level_up_interval: 10,
        },
    )
    .await
    .unwrap();

    let settings = db::get_discord_webhook_settings(&client, group_id)
        .await
        .unwrap();
    assert_eq!(settings.webhook_url, None);
}

#[tokio::test]
async fn test_discord_webhook_settings_scoped_per_group() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_a = create_test_group(&client, "discordtest4a").await;
    let group_b = create_test_group(&client, "discordtest4b").await;

    db::update_discord_webhook_settings(
        &client,
        group_a,
        &DiscordWebhookSettings {
            webhook_url: Some("https://discord.com/api/webhooks/111/aaa".to_string()),
            notify_kills: true,
            notify_deaths: true,
            notify_drops: true,
            drops_min_value: 250000,
            drops_unique_only: false,
            notify_raids: true,
            notify_combat_achievements: true,
            notify_collection_log: true,
            notify_quests: true,
            notify_diaries: true,
            notify_pets: true,
            notify_clues: true,
            notify_level_ups: true,
            level_up_interval: 10,
        },
    )
    .await
    .unwrap();

    let settings_b = db::get_discord_webhook_settings(&client, group_b)
        .await
        .unwrap();
    assert_eq!(
        settings_b.webhook_url, None,
        "group b's settings must be unaffected"
    );
}

// --- Leaderboard / Graphs-tab metric data ---
//
// Kept in this file (rather than a separate integration test binary) so these share
// `TEST_MUTEX` with the rest of the suite - separate `tests/*.rs` files are separate binaries,
// which cargo can run concurrently, and every one of them drops/recreates the shared
// `groupscape_test` schema, so a second binary racing this one is a real cross-process hazard,
// not just an in-process one.

/// 24 distinct values, one per stored skill slot, so a slot-index bug and a "sum everything"
/// bug produce different, checkable totals: slot i (1-indexed) gets value `i`, so the sum of
/// all 24 is 300, while any single slot's value is just its own index.
fn distinct_skills() -> Vec<i32> {
    (1..=24).collect()
}

async fn set_skills(client: &deadpool_postgres::Client, group_id: i64, name: &str, skills: &[i32]) {
    client
        .execute(
            "UPDATE groupscape.members SET skills=$1 WHERE group_id=$2 AND member_name=$3",
            &[&skills, &group_id, &name],
        )
        .await
        .unwrap();
}

/// `get_xp_leaderboard` reports live-minus-baseline, defaulting the baseline to the live value
/// itself (i.e. a diff of 0) when no history row exists yet for a member/window. Seeding an
/// all-zero baseline row in `skills_year` (read by the `AllTime` window) makes the reported
/// value equal the live read directly, which is what these tests want to assert on.
async fn seed_zero_baseline(client: &deadpool_postgres::Client, group_id: i64, name: &str) {
    let member_id: i64 = client
        .query_one(
            "SELECT member_id FROM groupscape.members WHERE group_id=$1 AND member_name=$2",
            &[&group_id, &name],
        )
        .await
        .unwrap()
        .get(0);
    let zeros = vec![0i32; 24];
    client
        .execute(
            "INSERT INTO groupscape.skills_year (member_id, time, skills) VALUES ($1, $2, $3)",
            &[&member_id, &(Utc::now() - Duration::days(365)), &zeros],
        )
        .await
        .unwrap();
}

fn metric_sample_kill(npc_name: &str, loot_item_id: i32) -> GameEvent {
    GameEvent::Kill(KillEvent {
        npc_id: 1,
        npc_name: npc_name.to_string(),
        world_x: 0,
        world_y: 0,
        plane: 0,
        world: 1,
        occurred_at: None,
        participants: None,
        loot: Some(vec![LootItem {
            item_id: loot_item_id,
            quantity: 1,
        }]),
        account_kc: None,
        event_id: None,
        sub_kills: None,
        delve_level: None,
    })
}

#[tokio::test]
async fn test_xp_leaderboard_overall_sums_all_24_skills_not_slot_one() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "xptest1").await;
    db::add_group_member(&client, group_id, "Zezima").await.unwrap();
    set_skills(&client, group_id, "Zezima", &distinct_skills()).await;
    seed_zero_baseline(&client, group_id, "Zezima").await;

    let raw = db::get_xp_leaderboard(&client, group_id, LeaderboardWindow::AllTime, None, Utc::now())
        .await
        .unwrap();
    let zezima = raw.iter().find(|(name, _)| name == "Zezima").unwrap();
    // Sum of 1..=24 is 300. The old (buggy) behavior would report 1 (skills[1] == Agility's slot).
    assert_eq!(zezima.1, 300, "Overall XP must sum all 24 skill slots, not read skills[1] alone");
}

#[tokio::test]
async fn test_xp_leaderboard_specific_skill_ranks_by_that_slot() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "xptest2").await;
    db::add_group_member(&client, group_id, "Zezima").await.unwrap();
    set_skills(&client, group_id, "Zezima", &distinct_skills()).await;
    seed_zero_baseline(&client, group_id, "Zezima").await;

    // Woodcutting is 1-indexed slot 23 in the stored array (see skill_array_index mapping).
    let raw = db::get_xp_leaderboard(
        &client,
        group_id,
        LeaderboardWindow::AllTime,
        Some("Woodcutting"),
        Utc::now(),
    )
    .await
    .unwrap();
    let zezima = raw.iter().find(|(name, _)| name == "Zezima").unwrap();
    assert_eq!(zezima.1, 23, "Woodcutting XP must read its own 1-indexed slot (23)");

    // Sailing trails the array at slot 24.
    let raw = db::get_xp_leaderboard(
        &client,
        group_id,
        LeaderboardWindow::AllTime,
        Some("Sailing"),
        Utc::now(),
    )
    .await
    .unwrap();
    let zezima = raw.iter().find(|(name, _)| name == "Zezima").unwrap();
    assert_eq!(zezima.1, 24, "Sailing XP must read its own 1-indexed slot (24)");
}

#[tokio::test]
async fn test_xp_leaderboard_unrecognized_skill_falls_back_to_overall() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "xptest3").await;
    db::add_group_member(&client, group_id, "Zezima").await.unwrap();
    set_skills(&client, group_id, "Zezima", &distinct_skills()).await;
    seed_zero_baseline(&client, group_id, "Zezima").await;

    let raw = db::get_xp_leaderboard(
        &client,
        group_id,
        LeaderboardWindow::AllTime,
        Some("NotARealSkill"),
        Utc::now(),
    )
    .await
    .unwrap();
    let zezima = raw.iter().find(|(name, _)| name == "Zezima").unwrap();
    assert_eq!(
        zezima.1, 300,
        "an unrecognized skill string should behave like Overall (sum), not error"
    );
}

#[tokio::test]
async fn test_aggregate_bank_value_and_get_bank_value_for_period_round_trip() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let mut client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "bankvaluetest1").await;
    db::add_group_member(&client, group_id, "Zezima").await.unwrap();
    client
        .execute(
            "UPDATE groupscape.members SET bank=$1 WHERE group_id=$2 AND member_name=$3",
            &[&vec![314i32, 10i32], &group_id, &"Zezima"],
        )
        .await
        .unwrap();

    let mut ge_prices = std::collections::HashMap::new();
    ge_prices.insert(314, 3i64); // 10 * 3 = 30

    let now = Utc::now();
    db::aggregate_bank_value(&mut client, &ge_prices, now)
        .await
        .expect("aggregate_bank_value should succeed");

    let day_data = db::get_bank_value_for_period(&client, group_id, db::AggregatePeriod::Day)
        .await
        .expect("get_bank_value_for_period should succeed");
    let zezima = day_data
        .iter()
        .find(|m| m.name == "Zezima")
        .expect("Zezima should have bank value data");
    assert_eq!(zezima.metric_data.len(), 1);
    assert_eq!(zezima.metric_data[0].value, 30);

    // Retention should not delete the only (most recent) row.
    db::apply_bank_value_retention(&mut client)
        .await
        .expect("apply_bank_value_retention should succeed");
    let day_data = db::get_bank_value_for_period(&client, group_id, db::AggregatePeriod::Day)
        .await
        .unwrap();
    assert_eq!(
        day_data.iter().find(|m| m.name == "Zezima").unwrap().metric_data.len(),
        1
    );
}

#[tokio::test]
async fn test_get_boss_kc_metric_data_is_cumulative_and_zero_fills_are_absent() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = create_test_pool().await;
    setup(&pool).await;
    let client = pool.get().await.unwrap();
    let group_id = create_test_group(&client, "bosskcmetrictest1").await;
    db::add_group_member(&client, group_id, "Zezima").await.unwrap();
    db::add_group_member(&client, group_id, "Woox").await.unwrap();
    let session_id = db::ensure_open_session(&client, group_id).await.unwrap();

    db::insert_activity_event(
        &client,
        group_id,
        session_id,
        "Zezima",
        &metric_sample_kill("Vorkath", 100),
    )
    .await
    .unwrap();
    db::insert_activity_event(
        &client,
        group_id,
        session_id,
        "Zezima",
        &metric_sample_kill("Vorkath", 100),
    )
    .await
    .unwrap();
    db::insert_activity_event(
        &client,
        group_id,
        session_id,
        "Zezima",
        &metric_sample_kill("Zulrah", 100),
    )
    .await
    .unwrap();

    let all_bosses = db::get_boss_kc_metric_data(&client, group_id, db::AggregatePeriod::Year, None)
        .await
        .unwrap();
    let zezima = all_bosses.iter().find(|m| m.name == "Zezima").unwrap();
    assert_eq!(
        zezima.metric_data.last().unwrap().value,
        3,
        "All Bosses should count every kill regardless of npc name"
    );
    assert!(
        all_bosses.iter().find(|m| m.name == "Woox").is_none(),
        "a member with zero matching events should be absent, not zero-filled"
    );

    let vorkath_only =
        db::get_boss_kc_metric_data(&client, group_id, db::AggregatePeriod::Year, Some("Vorkath"))
            .await
            .unwrap();
    let zezima = vorkath_only.iter().find(|m| m.name == "Zezima").unwrap();
    assert_eq!(zezima.metric_data.last().unwrap().value, 2);
}
