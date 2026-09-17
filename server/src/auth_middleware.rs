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

const AUTH_CACHE_TTL: Duration = Duration::from_secs(300);
const AUTH_CACHE_MAX_ENTRIES: usize = 10_000;
const AUTH_CACHE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Hash, Eq, PartialEq)]
struct AuthenticationCacheKey {
    group_name: String,
    token_hash: String,
}

struct CachedAuthentication {
    group_id: i64,
    expires_at: Instant,
}

struct AuthenticationCacheInner {
    entries: HashMap<AuthenticationCacheKey, CachedAuthentication>,
    last_sweep: Instant,
}

pub struct AuthenticationCache {
    inner: RwLock<AuthenticationCacheInner>,
}

impl AuthenticationCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(AuthenticationCacheInner {
                entries: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    fn get(&self, group_name: &str, token_hash: &str) -> Option<i64> {
        let inner = self.inner.read().ok()?;
        let key = AuthenticationCacheKey {
            group_name: group_name.to_owned(),
            token_hash: token_hash.to_owned(),
        };
        inner
            .entries
            .get(&key)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.group_id)
    }

    fn insert(&self, group_name: &str, token_hash: String, group_id: i64) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        let now = Instant::now();
        if now.duration_since(inner.last_sweep) >= AUTH_CACHE_SWEEP_INTERVAL {
            inner.entries.retain(|_, entry| entry.expires_at > now);
            inner.last_sweep = now;
        }
        if inner.entries.len() >= AUTH_CACHE_MAX_ENTRIES {
            return;
        }
        inner.entries.insert(
            AuthenticationCacheKey {
                group_name: group_name.to_owned(),
                token_hash,
            },
            CachedAuthentication {
                group_id,
                expires_at: now + AUTH_CACHE_TTL,
            },
        );
    }
}

impl Default for AuthenticationCache {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AuthenticateMiddlewareFactory {
    cache: Arc<AuthenticationCache>,
}
impl AuthenticateMiddlewareFactory {
    pub fn new(cache: Arc<AuthenticationCache>) -> Self {
        AuthenticateMiddlewareFactory { cache }
    }
}
impl<S, B> Transform<S, ServiceRequest> for AuthenticateMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: actix_web::body::MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthenticateMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthenticateMiddleware {
            service: Rc::new(service),
            cache: Arc::clone(&self.cache),
        }))
    }
}

pub struct AuthenticationResult {
    pub group_id: i64,
    /// `Some` only when this request came in through the character-key scope
    /// (`CharacterAuthenticateMiddleware`), which resolves the account_hash server-side from the
    /// URL + API key. `None` for the group-token dashboard scope, which has no notion of an
    /// individual account.
    pub account_hash: Option<String>,
    /// The specific character row the character-key auth middleware resolved this request to -
    /// always `Some` alongside `account_hash`. Since the same `account_hash` can now be linked to
    /// more than one account, this is what disambiguates which account's row a caller should use
    /// (re-deriving from `account_hash` alone would be ambiguous).
    pub character_id: Option<i64>,
    /// The DB `accounts.id` row the API key belongs to - always `Some` alongside `account_hash`,
    /// `None` for the group-token dashboard scope (same shape as `account_hash`/`character_id`).
    /// Lets a character-scope handler write an `account_id` FK (e.g. `chat_messages.account_id`)
    /// without a webapp session token, which the plugin never has - see the "!gs" chat spec's
    /// account_id resolution ticket.
    pub account_id: Option<i64>,
}
type AuthenticationInfo = Rc<AuthenticationResult>;
pub struct Authenticated(AuthenticationInfo);
impl std::ops::Deref for Authenticated {
    type Target = AuthenticationInfo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl FromRequest for Authenticated {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let value = req.extensions().get::<AuthenticationInfo>().cloned();
        let result = match value {
            Some(v) => Ok(Authenticated(v)),
            None => Err(actix_web::error::ErrorUnauthorized("")),
        };
        ready(result)
    }
}
pub struct AuthenticateMiddleware<S> {
    service: Rc<S>,
    cache: Arc<AuthenticationCache>,
}

/// Finds `key`'s value in a raw query string (`a=1&key=val&b=2`), URL-decoded. Used only for the
/// `/ws` route's `Authorization`-header fallback, above.
fn query_param(query_string: &str, key: &str) -> Option<String> {
    query_string.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            urlencoding::decode(v).ok().map(|v| v.into_owned())
        } else {
            None
        }
    })
}

/// Authenticate against the database on cache miss.
/// Returns Ok(group_id) on success, or an error response to return directly.
async fn authenticate_via_db(
    req: &ServiceRequest,
    group_name: &str,
    token: &str,
    token_hash: &str,
    cache: &AuthenticationCache,
) -> Result<i64, actix_web::Error> {
    let db_pool = req
        .app_data::<web::Data<Pool>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError(""))?;
    let client = db_pool
        .get()
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?;

    let group_id = db::get_group(&client, group_name, token)
        .await
        .map_err(|_| actix_web::error::ErrorUnauthorized(""))?;

    cache.insert(group_name, token_hash.to_owned(), group_id);
    Ok(group_id)
}

/// Fallback when the token in `Authorization` isn't a valid group token for this group: try it
/// as an account session token instead (same header, same field - there's no ambiguity to
/// resolve up front since a group token and a session token are drawn from unrelated random
/// spaces). Grants access only if the account has a confirmed character already linked to a
/// group with this name, so this can never grant access beyond what the account's own
/// characters are already members of. Lets a signed-in account view a group's dashboard on a
/// device that never had that group's token saved locally - see `db::find_group_id_for_account_character`.
async fn authenticate_via_account_session(
    req: &ServiceRequest,
    group_name: &str,
    token: &str,
    token_hash: &str,
    cache: &AuthenticationCache,
) -> Result<i64, actix_web::Error> {
    let db_pool = req
        .app_data::<web::Data<Pool>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError(""))?;
    let client = db_pool
        .get()
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?;

    let session_token_hash = crate::crypto::session_token_hash(token);
    let account = db::get_account_by_session_token_hash(&client, &session_token_hash)
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?
        .ok_or_else(|| actix_web::error::ErrorUnauthorized(""))?;

    let group_id = db::find_group_id_for_account_character(&client, account.id, group_name)
        .await
        .map_err(|_| actix_web::error::ErrorInternalServerError(""))?
        .ok_or_else(|| actix_web::error::ErrorUnauthorized(""))?;

    cache.insert(group_name, token_hash.to_owned(), group_id);
    Ok(group_id)
}

impl<S, B> Service<ServiceRequest> for AuthenticateMiddleware<S>
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

        async move {
            let group_name = match req.match_info().get("group_name") {
                Some(group_name) => group_name,
                None => {
                    return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                        "Missing group name from request",
                    )));
                }
            };

            if group_name != "_" {
                // The browser `WebSocket` constructor can't set an `Authorization` header (no
                // custom-header support on the upgrade request), so the group chat drawer's `/ws`
                // connection sends the group token as a `?token=` query param instead - the only
                // caller that hits this fallback, since every `fetch`-based request still sends
                // the header normally. Query params can end up in access logs / browser history
                // exactly like the group token already does in `?token=` invite links, so this
                // doesn't newly expose anything the token wasn't already exposed to.
                let owned_token;
                let token: &str = if let Some(auth_header) = req.headers().get("Authorization") {
                    match auth_header.to_str() {
                        Ok(token) => token,
                        Err(_) => {
                            return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                                "Unable to parse Authorization header",
                            )));
                        }
                    }
                } else if let Some(query_token) = query_param(req.query_string(), "token") {
                    owned_token = query_token;
                    owned_token.as_str()
                } else {
                    return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                        "Authorization header missing from request",
                    )));
                };

                let token_hash = crate::crypto::token_hash(token, group_name);
                let group_id = match cache.get(group_name, &token_hash) {
                    Some(group_id) => group_id,
                    None => match authenticate_via_db(&req, group_name, token, &token_hash, &cache)
                        .await
                    {
                        Ok(group_id) => group_id,
                        Err(_) => {
                            match authenticate_via_account_session(
                                &req,
                                group_name,
                                token,
                                &token_hash,
                                &cache,
                            )
                            .await
                            {
                                Ok(group_id) => group_id,
                                Err(e) => return Ok(req.error_response(e)),
                            }
                        }
                    },
                };

                let authentication_result = AuthenticationResult {
                    group_id,
                    account_hash: None,
                    character_id: None,
                    account_id: None,
                };
                req.extensions_mut()
                    .insert::<AuthenticationInfo>(Rc::new(authentication_result));
            }

            let res = srv.call(req).await?;
            Ok(res.map_into_boxed_body())
        }
        .boxed_local()
    }
}

#[cfg(test)]
mod tests {
    use super::{query_param, AuthenticationCache};

    #[test]
    fn caches_successful_authentication_by_group_and_token_hash() {
        let cache = AuthenticationCache::new();
        cache.insert("testgroup", "valid-token-hash".to_owned(), 42);

        assert_eq!(cache.get("testgroup", "valid-token-hash"), Some(42));
        assert_eq!(cache.get("testgroup", "other-token-hash"), None);
        assert_eq!(cache.get("other-group", "valid-token-hash"), None);
    }

    #[test]
    fn query_param_finds_key_among_others() {
        assert_eq!(
            query_param("a=1&token=abc123&b=2", "token"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn query_param_url_decodes_the_value() {
        assert_eq!(
            query_param("token=a%20b%2Bc", "token"),
            Some("a b+c".to_string())
        );
    }

    #[test]
    fn query_param_missing_key_returns_none() {
        assert_eq!(query_param("a=1&b=2", "token"), None);
        assert_eq!(query_param("", "token"), None);
    }
}
