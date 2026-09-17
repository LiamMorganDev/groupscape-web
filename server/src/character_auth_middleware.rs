use crate::auth_middleware::AuthenticationResult;
use crate::db;
use actix_web::{
    body::BoxBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    web, Error, FromRequest, HttpMessage, HttpRequest,
};
use deadpool_postgres::Pool;
use futures_util::{
    future::{ready, LocalBoxFuture, Ready},
    FutureExt,
};
use std::{
    collections::HashMap,
    rc::Rc,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

const CHARACTER_AUTH_CACHE_TTL: Duration = Duration::from_secs(60);
const CHARACTER_AUTH_CACHE_MAX_ENTRIES: usize = 10_000;
const CHARACTER_AUTH_CACHE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Hash, Eq, PartialEq)]
struct CharacterAuthenticationCacheKey {
    account_hash: String,
    api_key_hash: String,
}

#[derive(Clone)]
struct CachedCharacterAuth {
    character_id: i64,
    account_id: i64,
    group_id: Option<i64>,
}

struct CachedCharacterAuthEntry {
    value: CachedCharacterAuth,
    expires_at: Instant,
}

struct CharacterAuthenticationCacheInner {
    entries: HashMap<CharacterAuthenticationCacheKey, CachedCharacterAuthEntry>,
    last_sweep: Instant,
}

/// Caches `(account_hash, api_key_hash) -> (character_id, group_id)` lookups, same shape as the
/// group `AuthenticationCache`. Shorter TTL than the group cache (60s vs 300s) since a
/// character's group-link status changes more often than a group's identity does - a freshly
/// confirmed-and-grouped character should start accepting telemetry within a minute without a
/// server restart.
pub struct CharacterAuthenticationCache {
    inner: RwLock<CharacterAuthenticationCacheInner>,
}

impl CharacterAuthenticationCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(CharacterAuthenticationCacheInner {
                entries: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    fn get(&self, account_hash: &str, api_key_hash: &str) -> Option<CachedCharacterAuth> {
        let inner = self.inner.read().ok()?;
        let key = CharacterAuthenticationCacheKey {
            account_hash: account_hash.to_owned(),
            api_key_hash: api_key_hash.to_owned(),
        };
        inner
            .entries
            .get(&key)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.value.clone())
    }

    fn insert(&self, account_hash: &str, api_key_hash: String, value: CachedCharacterAuth) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(inner.last_sweep) >= CHARACTER_AUTH_CACHE_SWEEP_INTERVAL {
            inner.entries.retain(|_, entry| entry.expires_at > now);
            inner.last_sweep = now;
        }
        if inner.entries.len() >= CHARACTER_AUTH_CACHE_MAX_ENTRIES {
            return;
        }
        inner.entries.insert(
            CharacterAuthenticationCacheKey {
                account_hash: account_hash.to_owned(),
                api_key_hash,
            },
            CachedCharacterAuthEntry {
                value,
                expires_at: now + CHARACTER_AUTH_CACHE_TTL,
            },
        );
    }

    /// Invalidated by the confirm/remove/link-to-group flows, all of which change what the next
    /// telemetry request should resolve to before this entry's TTL would naturally expire.
    pub fn invalidate(&self, account_hash: &str) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        inner.entries.retain(|key, _| key.account_hash != account_hash);
    }
}

impl Default for CharacterAuthenticationCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A lighter identity than `AuthenticationResult` - carries `character_id`/`account_hash`
/// regardless of whether a group is linked yet, for routes exempted from the "must have a
/// group" gate (identify, portrait upload/fetch).
pub struct CharacterIdentity {
    pub character_id: i64,
    pub account_hash: String,
    pub group_id: Option<i64>,
}
type CharacterIdentityInfo = Rc<CharacterIdentity>;
pub struct CharacterIdentified(CharacterIdentityInfo);
impl std::ops::Deref for CharacterIdentified {
    type Target = CharacterIdentityInfo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl FromRequest for CharacterIdentified {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let value = req.extensions().get::<CharacterIdentityInfo>().cloned();
        let result = match value {
            Some(v) => Ok(CharacterIdentified(v)),
            None => Err(actix_web::error::ErrorUnauthorized("")),
        };
        ready(result)
    }
}

pub struct CharacterAuthenticateMiddlewareFactory {
    cache: Arc<CharacterAuthenticationCache>,
    requires_group: bool,
}
impl CharacterAuthenticateMiddlewareFactory {
    pub fn new(cache: Arc<CharacterAuthenticationCache>, requires_group: bool) -> Self {
        CharacterAuthenticateMiddlewareFactory {
            cache,
            requires_group,
        }
    }
}
impl<S, B> Transform<S, ServiceRequest> for CharacterAuthenticateMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: actix_web::body::MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = CharacterAuthenticateMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(CharacterAuthenticateMiddleware {
            service: Rc::new(service),
            cache: Arc::clone(&self.cache),
            requires_group: self.requires_group,
        }))
    }
}

pub struct CharacterAuthenticateMiddleware<S> {
    service: Rc<S>,
    cache: Arc<CharacterAuthenticationCache>,
    requires_group: bool,
}

/// Resolves `api_key -> account -> character (by account_hash)`, auto-creating a pending
/// character on first sight (capped, denylist-checked). Returns the resolved
/// `(character_id, group_id)` on success, or an error response to return directly.
async fn authenticate_via_db(
    req: &ServiceRequest,
    account_hash: &str,
    api_key_hash: &str,
    cache: &CharacterAuthenticationCache,
) -> Result<CachedCharacterAuth, actix_web::Error> {
    let db_pool = req
        .app_data::<web::Data<Pool>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError(""))?;
    let client = db_pool
        .get()
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?;

    let account = db::get_account_by_api_key_hash(&client, api_key_hash)
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?
        .ok_or_else(|| actix_web::error::ErrorUnauthorized(""))?;

    let character = match db::find_character_by_account_and_hash(&client, account.id, account_hash)
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?
    {
        Some(character) => character,
        None => {
            let denylisted = db::is_character_denylisted(&client, account.id, account_hash)
                .await
                .map_err(|_| actix_web::error::ErrorInternalServerError(""))?;
            if denylisted {
                return Err(actix_web::error::ErrorUnauthorized(""));
            }
            let character_count = db::count_characters_for_account(&client, account.id)
                .await
                .map_err(|_| actix_web::error::ErrorInternalServerError(""))?;
            if character_count >= db::CHARACTER_CAP_PER_ACCOUNT {
                return Err(actix_web::error::ErrorUnauthorized(""));
            }
            db::create_pending_character(&client, account.id, account_hash)
                .await
                .map_err(|_| actix_web::error::ErrorInternalServerError(""))?
        }
    };

    let group_id = db::find_character_group_link(&client, character.id)
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?
        .map(|link| link.group_id);

    let value = CachedCharacterAuth {
        character_id: character.id,
        account_id: account.id,
        group_id,
    };
    cache.insert(account_hash, api_key_hash.to_owned(), value.clone());
    Ok(value)
}

impl<S, B> Service<ServiceRequest> for CharacterAuthenticateMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: actix_web::body::MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;
    fn poll_ready(
        &self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let srv = Rc::clone(&self.service);
        let cache = Arc::clone(&self.cache);
        let requires_group = self.requires_group;

        async move {
            let account_hash = match req.match_info().get("account_hash") {
                Some(account_hash) => account_hash.to_owned(),
                None => {
                    return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                        "Missing account hash from request",
                    )));
                }
            };

            let auth_header = match req.headers().get("Authorization") {
                Some(auth_header) => auth_header,
                None => {
                    return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                        "Authorization header missing from request",
                    )));
                }
            };
            let api_key = match auth_header.to_str() {
                Ok(api_key) => api_key.to_owned(),
                Err(_) => {
                    return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                        "Unable to parse Authorization header",
                    )));
                }
            };

            let api_key_hash = crate::crypto::api_key_hash(&api_key);
            let resolved = match cache.get(&account_hash, &api_key_hash) {
                Some(resolved) => resolved,
                None => {
                    match authenticate_via_db(&req, &account_hash, &api_key_hash, &cache).await {
                        Ok(resolved) => resolved,
                        Err(e) => return Ok(req.error_response(e)),
                    }
                }
            };

            if requires_group && resolved.group_id.is_none() {
                return Ok(req.error_response(actix_web::error::ErrorForbidden(
                    "Character is not linked to a group",
                )));
            }

            if requires_group {
                let authentication_result = AuthenticationResult {
                    group_id: resolved.group_id.expect("checked above"),
                    account_hash: Some(account_hash.clone()),
                    character_id: Some(resolved.character_id),
                    account_id: Some(resolved.account_id),
                };
                req.extensions_mut()
                    .insert::<Rc<AuthenticationResult>>(Rc::new(authentication_result));
            } else {
                let identity = CharacterIdentity {
                    character_id: resolved.character_id,
                    account_hash: account_hash.clone(),
                    group_id: resolved.group_id,
                };
                req.extensions_mut()
                    .insert::<CharacterIdentityInfo>(Rc::new(identity));
            }

            let res = srv.call(req).await?;
            Ok(res.map_into_boxed_body())
        }
        .boxed_local()
    }
}

#[cfg(test)]
mod tests {
    use super::{CachedCharacterAuth, CharacterAuthenticationCache};

    #[test]
    fn caches_successful_authentication_by_account_hash_and_key_hash() {
        let cache = CharacterAuthenticationCache::new();
        cache.insert(
            "12345",
            "valid-key-hash".to_owned(),
            CachedCharacterAuth {
                character_id: 42,
                account_id: 99,
                group_id: Some(7),
            },
        );

        let cached = cache.get("12345", "valid-key-hash").unwrap();
        assert_eq!(cached.character_id, 42);
        assert_eq!(cached.account_id, 99);
        assert_eq!(cached.group_id, Some(7));
        assert!(cache.get("12345", "other-key-hash").is_none());
        assert!(cache.get("other-hash", "valid-key-hash").is_none());
    }

    #[test]
    fn invalidate_clears_all_entries_for_an_account_hash() {
        let cache = CharacterAuthenticationCache::new();
        cache.insert(
            "12345",
            "key-hash".to_owned(),
            CachedCharacterAuth {
                character_id: 42,
                account_id: 99,
                group_id: None,
            },
        );
        cache.invalidate("12345");
        assert!(cache.get("12345", "key-hash").is_none());
    }
}
