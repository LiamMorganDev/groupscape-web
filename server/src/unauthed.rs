use crate::config::Config;
use crate::db;
use crate::error::ApiError;
use crate::models::{CaptchaVerifyResponse, CreateGroup, GEPrices, WikiGEPrices};
use crate::validators::valid_name;
use actix_web::{get, post, web, Error, HttpResponse};
use arc_swap::{ArcSwap, ArcSwapAny};
use deadpool_postgres::{Client, Pool};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::{task, time};

static GE_PRICES: LazyLock<ArcSwapAny<Arc<String>>> =
    LazyLock::new(|| ArcSwap::from(Arc::new(String::default())));
pub async fn fetch_latest_prices() -> Result<WikiGEPrices, ApiError> {
    let wiki_ge_prices = task::spawn_blocking(|| {
        ureq::get("https://prices.runescape.wiki/api/v1/osrs/latest")
            .header("User-Agent", "GroupScape - Dprk#8740")
            .call()
            .map_err(ApiError::UreqError)?
            .body_mut()
            .read_json::<WikiGEPrices>()
            .map_err(ApiError::UreqError)
    })
    .await
    .unwrap()?;

    Ok(wiki_ge_prices)
}

pub async fn update_ge_prices() -> Result<(), ApiError> {
    let wiki_ge_prices = fetch_latest_prices().await?;
    let mut ge_prices: GEPrices = std::collections::HashMap::new();
    for (item_id, wiki_ge_price) in wiki_ge_prices.data {
        let mut avg_ge_price: i64 = 0;
        if let Some(high) = wiki_ge_price.high {
            avg_ge_price = high
        }
        if let Some(low) = wiki_ge_price.low {
            if avg_ge_price > 0 {
                avg_ge_price = (avg_ge_price + low) / 2
            } else {
                avg_ge_price = low
            }
        }

        ge_prices.insert(item_id, avg_ge_price);
    }

    // Coins (995) aren't a tradeable GE item, so the wiki price feed never lists them - but they're
    // worth exactly 1gp each by definition. Without this every gp-value calc that joins against
    // this map (loot summary, gp-earned leaderboard, bank/inventory value) silently prices coin
    // drops/holdings at 0.
    ge_prices.insert(995, 1);

    GE_PRICES.store(Arc::new(serde_json::to_string(&ge_prices)?));

    Ok(())
}

pub fn start_ge_updater() {
    task::spawn(async {
        let mut interval = time::interval(Duration::from_secs(14400));

        loop {
            interval.tick().await;
            log::info!("Fetching latest ge prices");

            match update_ge_prices().await {
                Ok(_) => (),
                Err(err) => {
                    log::error!("Failed to fetch latest ge prices: {}", err);
                }
            }
        }
    });
}

pub fn start_skills_aggregator(db_pool: Pool) {
    task::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(1800));

        loop {
            interval.tick().await;
            log::info!("Running skill aggregator");

            match db_pool.get().await {
                Ok(mut client) => {
                    match db::aggregate_skills(&mut client).await {
                        Ok(_) => (),
                        Err(err) => {
                            log::error!("Failed to aggregate skills: {}", err);
                        }
                    }

                    match db::apply_skills_retention(&mut client).await {
                        Ok(_) => (),
                        Err(err) => {
                            log::error!("Failed to apply skills retention: {}", err);
                        }
                    }
                }
                Err(err) => {
                    log::error!("Failed to get db client: {}", err);
                }
            }
        }
    });
}

/// Captures each member's current bank value once per UTC day, feeding the GP-earned
/// leaderboard metric ([`crate::leaderboard`]) - runs far more often than "once a day" (same
/// margin as `start_session_idle_closer`) purely so a server that was down at midnight still
/// picks today's snapshot up soon after; the upsert on `(member_id, snapshot_date)` makes
/// repeat runs within the same day a no-op refresh rather than a duplicate row.
pub fn start_bank_value_snapshotter(db_pool: Pool) {
    task::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(1800));

        loop {
            interval.tick().await;
            log::info!("Capturing bank value snapshots");

            match db_pool.get().await {
                Ok(client) => {
                    let ge_prices = get_ge_prices_map();
                    if let Err(err) =
                        db::capture_bank_value_snapshots(&client, &ge_prices, chrono::Utc::now()).await
                    {
                        log::error!("Failed to capture bank value snapshots: {}", err);
                    }
                }
                Err(err) => {
                    log::error!("Failed to get db client: {}", err);
                }
            }
        }
    });
}

/// Feeds the Graphs tab's GP-earned chart history (`bank_value_day`/`bank_value_month`/
/// `bank_value_year`) - a parallel, purely-additive job to `start_bank_value_snapshotter` above,
/// which stays daily-only and keeps serving the GP-earned *leaderboard* metric unchanged. Same
/// 1800s interval and log-and-continue-on-error style as `start_skills_aggregator`.
pub fn start_bank_value_aggregator(db_pool: Pool) {
    task::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(1800));

        loop {
            interval.tick().await;
            log::info!("Running bank value aggregator");

            match db_pool.get().await {
                Ok(mut client) => {
                    let ge_prices = get_ge_prices_map();
                    match db::aggregate_bank_value(&mut client, &ge_prices, chrono::Utc::now())
                        .await
                    {
                        Ok(_) => (),
                        Err(err) => {
                            log::error!("Failed to aggregate bank value: {}", err);
                        }
                    }

                    match db::apply_bank_value_retention(&mut client).await {
                        Ok(_) => (),
                        Err(err) => {
                            log::error!("Failed to apply bank value retention: {}", err);
                        }
                    }
                }
                Err(err) => {
                    log::error!("Failed to get db client: {}", err);
                }
            }
        }
    });
}

/// Mirrors `groupscape-old`'s `closeIdleSessions` recurring job: closes any session that's
/// gone 15 minutes without a heartbeat from its group. Runs far more often than that window
/// (every minute) so a session closes promptly after it actually goes idle.
pub fn start_session_idle_closer(db_pool: Pool) {
    task::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(60));
        let idle_after = chrono::Duration::minutes(15);

        loop {
            interval.tick().await;

            match db_pool.get().await {
                Ok(client) => match db::close_idle_sessions(&client, idle_after).await {
                    Ok(closed) if closed > 0 => {
                        log::info!("Closed {} idle session(s)", closed);
                        if let Err(err) = db::prune_old_sessions(&client).await {
                            log::error!("Failed to prune old sessions: {}", err);
                        }
                    }
                    Ok(_) => {
                        if let Err(err) = db::prune_old_sessions(&client).await {
                            log::error!("Failed to prune old sessions: {}", err);
                        }
                    }
                    Err(err) => {
                        log::error!("Failed to close idle sessions: {}", err);
                    }
                },
                Err(err) => {
                    log::error!("Failed to get db client: {}", err);
                }
            }
        }
    });
}

/// Purges loot log entries (`groupscape.activity_events` rows with `event_type = 'loot'`)
/// older than 28 days. Runs hourly, matching the coarser-interval jobs above.
pub fn start_loot_log_cleanup(db_pool: Pool) {
    task::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(3600));

        loop {
            interval.tick().await;

            match db_pool.get().await {
                Ok(client) => match db::prune_old_loot_events(&client).await {
                    Ok(pruned) if pruned > 0 => {
                        log::info!("Pruned {} old loot log entries", pruned);
                    }
                    Ok(_) => (),
                    Err(err) => {
                        log::error!("Failed to prune old loot log entries: {}", err);
                    }
                },
                Err(err) => {
                    log::error!("Failed to get db client: {}", err);
                }
            }
        }
    });
}

/// Purges `groupscape.chat_messages` rows older than `CHAT_HISTORY_DAYS` - keeps the table
/// bounded now that `get_chat_messages` always reads the same rolling window rather than
/// advancing a per-account delivery cursor. Runs hourly, matching `start_loot_log_cleanup`.
pub fn start_chat_cleanup(db_pool: Pool) {
    task::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(3600));

        loop {
            interval.tick().await;

            match db_pool.get().await {
                Ok(client) => match db::prune_old_chat_messages(&client).await {
                    Ok(pruned) if pruned > 0 => {
                        log::info!("Pruned {} old chat message(s)", pruned);
                    }
                    Ok(_) => (),
                    Err(err) => {
                        log::error!("Failed to prune old chat messages: {}", err);
                    }
                },
                Err(err) => {
                    log::error!("Failed to get db client: {}", err);
                }
            }
        }
    });
}

/// Parses the same cached GE-price snapshot `GET /ge-prices` serves as raw JSON into a map,
/// for server-side joins (e.g. loot summary/split value calculations) instead of re-fetching.
pub fn get_ge_prices_map() -> GEPrices {
    let ge_prices_opt = GE_PRICES.load();
    serde_json::from_str(&ge_prices_opt).unwrap_or_default()
}

#[get("/ge-prices")]
pub async fn get_ge_prices() -> Result<HttpResponse, Error> {
    let ge_prices_opt = GE_PRICES.load();
    let res: String = (**ge_prices_opt).clone();

    Ok(HttpResponse::Ok()
        .append_header(("Cache-Control", "public, max-age=86400"))
        .content_type("application/json")
        .body(res))
}

pub async fn verify_captcha(
    response: &str,
    secret: &str,
) -> Result<CaptchaVerifyResponse, ApiError> {
    let response = response.to_owned();
    let secret = secret.to_owned();
    let captcha_verify_response = task::spawn_blocking(move || {
        ureq::post("https://hcaptcha.com/siteverify")
            .send_form([("response", response.as_str()), ("secret", secret.as_str())])
            .map_err(ApiError::UreqError)?
            .body_mut()
            .read_json::<CaptchaVerifyResponse>()
            .map_err(ApiError::UreqError)
    })
    .await
    .unwrap()?;

    Ok(captcha_verify_response)
}

#[post("/create-group")]
pub async fn create_group(
    create_group: web::Json<CreateGroup>,
    db_pool: web::Data<Pool>,
    config: web::Data<Config>,
) -> Result<HttpResponse, Error> {
    let mut create_group_inner = create_group.into_inner();

    if config.hcaptcha.enabled {
        let captcha_verify_response = verify_captcha(
            &create_group_inner.captcha_response,
            &config.hcaptcha.secret,
        )
        .await?;
        if !captcha_verify_response.success {
            return Ok(HttpResponse::BadRequest().body("Captcha response verification failed"));
        }
    }

    if create_group_inner.member_names.len() > 5 {
        return Ok(HttpResponse::BadRequest().body("Too many member names provided"));
    }

    create_group_inner.name = create_group_inner.name.trim().to_string();
    if !valid_name(&create_group_inner.name) {
        return Ok(HttpResponse::BadRequest().body("Provided group name is not valid"));
    }

    // Embed the group name in the token so the RuneLite plugin can derive it locally
    // instead of requiring the user to enter it separately.
    create_group_inner.token = format!(
        "{}|{}",
        create_group_inner.name,
        uuid::Uuid::new_v4().hyphenated()
    );

    create_group_inner
        .member_names
        .retain(|member_name| !member_name.trim().is_empty());
    for member_name in &create_group_inner.member_names {
        if !valid_name(member_name) {
            return Ok(HttpResponse::BadRequest()
                .body(format!("Member name {} is not valid", member_name)));
        }
    }

    let mut client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    db::create_group(&mut client, &create_group_inner).await?;
    Ok(HttpResponse::Created().json(&create_group_inner))
}

#[get("captcha-enabled")]
pub async fn captcha_enabled(config: web::Data<Config>) -> Result<HttpResponse, Error> {
    Ok(HttpResponse::Ok().json(&config.hcaptcha))
}
