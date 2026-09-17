use crate::config::Config;
use actix_web::{
    body::BoxBody,
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    web, Error, FromRequest, HttpMessage, HttpRequest,
};
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

const ADMIN_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(300);
const ADMIN_RATE_LIMIT_MAX_ATTEMPTS: u32 = 5;
const ADMIN_RATE_LIMIT_LOCKOUT: Duration = Duration::from_secs(900);
const ADMIN_RATE_LIMIT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

struct AttemptRecord {
    failed_count: u32,
    window_start: Instant,
    locked_until: Option<Instant>,
}

struct AdminLoginRateLimiterInner {
    attempts: HashMap<String, AttemptRecord>,
    last_sweep: Instant,
}

/// Throttles admin-login attempts by client IP. Kept purely in-memory - the
/// admin panel has a single caller class (a human typing a token), so an
/// in-process HashMap is sufficient; a distributed limiter (Redis) is a
/// separate, cross-cutting concern tracked elsewhere.
pub struct AdminLoginRateLimiter {
    inner: RwLock<AdminLoginRateLimiterInner>,
}

impl AdminLoginRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(AdminLoginRateLimiterInner {
                attempts: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// Returns true if this key is currently locked out.
    fn is_locked_out(&self, key: &str) -> bool {
        let Ok(inner) = self.inner.read() else {
            return false;
        };
        inner
            .attempts
            .get(key)
            .and_then(|record| record.locked_until)
            .is_some_and(|locked_until| locked_until > Instant::now())
    }

    fn sweep_if_due(inner: &mut AdminLoginRateLimiterInner, now: Instant) {
        if now.duration_since(inner.last_sweep) < ADMIN_RATE_LIMIT_SWEEP_INTERVAL {
            return;
        }
        inner.attempts.retain(|_, record| {
            record
                .locked_until
                .map(|locked_until| locked_until > now)
                .unwrap_or_else(|| now.duration_since(record.window_start) < ADMIN_RATE_LIMIT_WINDOW)
        });
        inner.last_sweep = now;
    }

    /// Records a failed attempt, locking the key out once it crosses the threshold.
    fn record_failure(&self, key: &str) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        let now = Instant::now();
        Self::sweep_if_due(&mut inner, now);

        let record = inner.attempts.entry(key.to_owned()).or_insert(AttemptRecord {
            failed_count: 0,
            window_start: now,
            locked_until: None,
        });

        if now.duration_since(record.window_start) >= ADMIN_RATE_LIMIT_WINDOW {
            record.failed_count = 0;
            record.window_start = now;
            record.locked_until = None;
        }

        record.failed_count += 1;
        if record.failed_count >= ADMIN_RATE_LIMIT_MAX_ATTEMPTS {
            record.locked_until = Some(now + ADMIN_RATE_LIMIT_LOCKOUT);
        }
    }

    fn record_success(&self, key: &str) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        inner.attempts.remove(key);
    }
}

impl Default for AdminLoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

fn bearer_token(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.to_string())
}

fn client_key(req: &HttpRequest) -> String {
    req.connection_info()
        .realip_remote_addr()
        .unwrap_or("unknown")
        .to_string()
}

/// Shared by every admin-gated middleware: validates the `Authorization: Bearer <admin token>`
/// header against `config.admin.token_hash`, honoring the same IP lockout as the admin panel
/// login. Returns an error response to return directly on failure.
fn authorize_admin_bearer(
    req: &ServiceRequest,
    config: &Config,
    rate_limiter: &AdminLoginRateLimiter,
) -> Result<(), actix_web::Error> {
    if !config.admin.enabled {
        return Err(actix_web::error::ErrorNotFound(""));
    }

    let key = client_key(req.request());
    if rate_limiter.is_locked_out(&key) {
        return Err(actix_web::error::ErrorTooManyRequests(""));
    }

    let token = bearer_token(req.request());
    let presented_hash = token
        .as_deref()
        .map(|t| crate::crypto::token_hash(t, "admin"));

    let authorized = presented_hash
        .as_deref()
        .is_some_and(|hash| hash == config.admin.token_hash && !config.admin.token_hash.is_empty());

    if !authorized {
        rate_limiter.record_failure(&key);
        return Err(actix_web::error::ErrorUnauthorized(""));
    }
    rate_limiter.record_success(&key);
    Ok(())
}

pub struct AdminAuthenticateMiddlewareFactory {
    config: web::Data<Config>,
    rate_limiter: Arc<AdminLoginRateLimiter>,
}
impl AdminAuthenticateMiddlewareFactory {
    pub fn new(config: web::Data<Config>, rate_limiter: Arc<AdminLoginRateLimiter>) -> Self {
        AdminAuthenticateMiddlewareFactory {
            config,
            rate_limiter,
        }
    }
}
impl<S, B> Transform<S, ServiceRequest> for AdminAuthenticateMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: actix_web::body::MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = AdminAuthenticateMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AdminAuthenticateMiddleware {
            service: Rc::new(service),
            config: self.config.clone(),
            rate_limiter: Arc::clone(&self.rate_limiter),
        }))
    }
}

pub struct AdminAuthenticationResult;
type AdminAuthenticationInfo = Rc<AdminAuthenticationResult>;
pub struct AdminAuthenticated(#[allow(dead_code)] AdminAuthenticationInfo);
impl FromRequest for AdminAuthenticated {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let value = req.extensions().get::<AdminAuthenticationInfo>().cloned();
        let result = match value {
            Some(v) => Ok(AdminAuthenticated(v)),
            None => Err(actix_web::error::ErrorUnauthorized("")),
        };
        ready(result)
    }
}

pub struct AdminAuthenticateMiddleware<S> {
    service: Rc<S>,
    config: web::Data<Config>,
    rate_limiter: Arc<AdminLoginRateLimiter>,
}

impl<S, B> Service<ServiceRequest> for AdminAuthenticateMiddleware<S>
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
        let config = self.config.clone();
        let rate_limiter = Arc::clone(&self.rate_limiter);

        async move {
            if let Err(e) = authorize_admin_bearer(&req, &config, &rate_limiter) {
                return Ok(req.error_response(e));
            }

            req.extensions_mut()
                .insert::<AdminAuthenticationInfo>(Rc::new(AdminAuthenticationResult));

            let res = srv.call(req).await?;
            Ok(res.map_into_boxed_body())
        }
        .boxed_local()
    }
}

/// Lets a global admin read a group's live dashboard exactly as a member would, without ever
/// holding (or needing) that group's token. Gated by the same admin bearer token as the rest of
/// `/api/admin`, keyed by `{group_id}` (admin panel already deals in ids, not names). On success
/// this inserts the same `Authenticated` extension the group-token dashboard scope produces, so
/// the read-only `authed::` handlers mounted under it are reused completely unchanged - they only
/// ever read `auth.group_id` and never write session/activity rows, so a visit here leaves no
/// trace the group's members can see.
pub struct AdminGroupViewMiddlewareFactory {
    config: web::Data<Config>,
    rate_limiter: Arc<AdminLoginRateLimiter>,
}
impl AdminGroupViewMiddlewareFactory {
    pub fn new(config: web::Data<Config>, rate_limiter: Arc<AdminLoginRateLimiter>) -> Self {
        AdminGroupViewMiddlewareFactory {
            config,
            rate_limiter,
        }
    }
}
impl<S, B> Transform<S, ServiceRequest> for AdminGroupViewMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: actix_web::body::MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type InitError = ();
    type Transform = AdminGroupViewMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AdminGroupViewMiddleware {
            service: Rc::new(service),
            config: self.config.clone(),
            rate_limiter: Arc::clone(&self.rate_limiter),
        }))
    }
}

pub struct AdminGroupViewMiddleware<S> {
    service: Rc<S>,
    config: web::Data<Config>,
    rate_limiter: Arc<AdminLoginRateLimiter>,
}

impl<S, B> Service<ServiceRequest> for AdminGroupViewMiddleware<S>
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
        let config = self.config.clone();
        let rate_limiter = Arc::clone(&self.rate_limiter);

        async move {
            if let Err(e) = authorize_admin_bearer(&req, &config, &rate_limiter) {
                return Ok(req.error_response(e));
            }

            let group_id = match req
                .match_info()
                .get("group_id")
                .and_then(|v| v.parse::<i64>().ok())
            {
                Some(group_id) => group_id,
                None => {
                    return Ok(req.error_response(actix_web::error::ErrorBadRequest(
                        "Missing or invalid group id",
                    )));
                }
            };

            req.extensions_mut()
                .insert::<Rc<crate::auth_middleware::AuthenticationResult>>(Rc::new(
                    crate::auth_middleware::AuthenticationResult {
                        group_id,
                        account_hash: None,
                        character_id: None,
                        account_id: None,
                    },
                ));

            let res = srv.call(req).await?;
            Ok(res.map_into_boxed_body())
        }
        .boxed_local()
    }
}

#[cfg(test)]
mod tests {
    use super::AdminLoginRateLimiter;

    #[test]
    fn locks_out_after_max_failed_attempts() {
        let limiter = AdminLoginRateLimiter::new();
        for _ in 0..4 {
            limiter.record_failure("1.2.3.4");
            assert!(!limiter.is_locked_out("1.2.3.4"));
        }
        limiter.record_failure("1.2.3.4");
        assert!(limiter.is_locked_out("1.2.3.4"));

        // Unrelated key is unaffected.
        assert!(!limiter.is_locked_out("5.6.7.8"));
    }

    #[test]
    fn success_clears_recorded_failures() {
        let limiter = AdminLoginRateLimiter::new();
        limiter.record_failure("1.2.3.4");
        limiter.record_failure("1.2.3.4");
        limiter.record_success("1.2.3.4");

        for _ in 0..4 {
            limiter.record_failure("1.2.3.4");
        }
        assert!(!limiter.is_locked_out("1.2.3.4"));
    }
}
