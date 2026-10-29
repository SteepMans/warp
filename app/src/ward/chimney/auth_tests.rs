use std::collections::VecDeque;
use std::sync::Arc;

use super::*;

const BASE: &str = "https://chimney.example";

/// Scripted login and refresh responses; records every call.
#[derive(Default)]
struct FakeTransport {
    logins: Mutex<VecDeque<Result<TokenResponse, TransportError>>>,
    refreshes: Mutex<VecDeque<Result<TokenResponse, TransportError>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeTransport {
    fn with_logins(logins: Vec<Result<TokenResponse, TransportError>>) -> Self {
        Self {
            logins: Mutex::new(logins.into()),
            ..Default::default()
        }
    }

    fn push_refresh(&self, response: Result<TokenResponse, TransportError>) {
        self.refreshes.lock().push_back(response);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
}

#[async_trait]
impl AuthTransport for FakeTransport {
    async fn login(
        &self,
        base_url: &str,
        email: &str,
        password: &str,
    ) -> Result<TokenResponse, TransportError> {
        assert_eq!(base_url, BASE);
        self.calls.lock().push(format!("login {email} {password}"));
        // Stay pending once, so concurrent callers really overlap.
        YieldNow(false).await;
        self.logins
            .lock()
            .pop_front()
            .expect("unexpected login call")
    }

    async fn refresh(
        &self,
        base_url: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse, TransportError> {
        assert_eq!(base_url, BASE);
        self.calls.lock().push(format!("refresh {refresh_token}"));
        self.refreshes
            .lock()
            .pop_front()
            .expect("unexpected refresh call")
    }
}

struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

fn tokens(access: &str, refresh: &str, expires_in: u64) -> Result<TokenResponse, TransportError> {
    Ok(TokenResponse {
        access_token: Some(access.to_owned()),
        refresh_token: Some(refresh.to_owned()),
        expires_in: Some(expires_in),
        requires_2fa: Some(false),
    })
}

fn account() -> Credentials {
    Credentials::Password {
        email: "bot@example.com".to_owned(),
        password: "pw".to_owned(),
    }
}

/// A clock tests move by hand.
#[derive(Clone)]
struct Clock(Arc<Mutex<Instant>>);

impl Clock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn advance(&self, secs: u64) {
        *self.0.lock() += Duration::from_secs(secs);
    }

    fn now_fn(&self) -> impl Fn() -> Instant + Sync + use<> {
        let inner = self.0.clone();
        move || *inner.lock()
    }
}

#[derive(Debug, PartialEq)]
enum TestError {
    Auth(AuthError),
    Unauthorized,
}

impl AuthorizedError for TestError {
    fn from_auth(err: AuthError) -> Self {
        Self::Auth(err)
    }

    fn is_unauthorized(&self) -> bool {
        *self == Self::Unauthorized
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
}

#[test]
fn logs_in_once_and_reuses_the_token_while_fresh() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a1".to_owned()));
    clock.advance(800);
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a1".to_owned()));
    assert_eq!(transport.calls(), vec!["login bot@example.com pw"]);
}

#[test]
fn refreshes_a_minute_before_expiry() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    transport.push_refresh(tokens("a2", "r2", 900));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    block_on(auth.token(BASE, &creds, &transport, &now)).unwrap();
    clock.advance(841);
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a2".to_owned()));
    assert_eq!(transport.calls(), vec!["login bot@example.com pw", "refresh r1"]);
}

#[test]
fn refresh_keeps_the_old_refresh_token_when_none_comes_back() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    transport.push_refresh(Ok(TokenResponse {
        access_token: Some("a2".to_owned()),
        ..Default::default()
    }));
    transport.push_refresh(tokens("a3", "r3", 900));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    block_on(auth.token(BASE, &creds, &transport, &now)).unwrap();
    clock.advance(900);
    // No expiresIn: the default 15 minutes apply.
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a2".to_owned()));
    clock.advance(900);
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a3".to_owned()));
    assert_eq!(
        transport.calls(),
        vec!["login bot@example.com pw", "refresh r1", "refresh r1"]
    );
}

#[test]
fn rejected_refresh_falls_back_to_login() {
    let auth = Auth::default();
    let transport =
        FakeTransport::with_logins(vec![tokens("a1", "r1", 900), tokens("a2", "r2", 900)]);
    transport.push_refresh(Err(TransportError::Status(401)));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    block_on(auth.token(BASE, &creds, &transport, &now)).unwrap();
    clock.advance(900);
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a2".to_owned()));
    assert_eq!(
        transport.calls(),
        vec!["login bot@example.com pw", "refresh r1", "login bot@example.com pw"]
    );
}

#[test]
fn network_error_on_refresh_does_not_log_in() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    transport.push_refresh(Err(TransportError::Network));
    transport.push_refresh(tokens("a2", "r2", 900));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    block_on(auth.token(BASE, &creds, &transport, &now)).unwrap();
    clock.advance(900);
    assert_eq!(
        block_on(auth.token(BASE, &creds, &transport, &now)),
        Err(AuthError::Network)
    );
    // The refresh token survives a network error.
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a2".to_owned()));
}

#[test]
fn login_errors_are_reported() {
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();
    let cases = [
        (Err(TransportError::Status(401)), AuthError::LoginRejected(401)),
        (Err(TransportError::Status(500)), AuthError::LoginRejected(500)),
        (Err(TransportError::Network), AuthError::Network),
        (Err(TransportError::Parse), AuthError::Parse),
        (
            Ok(TokenResponse {
                requires_2fa: Some(true),
                ..Default::default()
            }),
            AuthError::TwoFactorRequired,
        ),
        (Ok(TokenResponse::default()), AuthError::NoAccessToken),
    ];
    for (response, expected) in cases {
        let auth = Auth::default();
        let transport = FakeTransport::with_logins(vec![response]);
        assert_eq!(
            block_on(auth.token(BASE, &creds, &transport, &now)),
            Err(expected)
        );
    }
}

#[test]
fn static_token_is_used_without_auth_calls() {
    let auth = Auth::default();
    let transport = FakeTransport::default();
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = Credentials::Token("static".to_owned());

    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("static".to_owned()));
    assert!(transport.calls().is_empty());
}

#[test]
fn changed_credentials_sign_in_again() {
    let auth = Auth::default();
    let transport =
        FakeTransport::with_logins(vec![tokens("a1", "r1", 900), tokens("b1", "r2", 900)]);
    let clock = Clock::new();
    let now = clock.now_fn();

    block_on(auth.token(BASE, &account(), &transport, &now)).unwrap();
    let other = Credentials::Password {
        email: "bot@example.com".to_owned(),
        password: "new".to_owned(),
    };
    assert_eq!(block_on(auth.token(BASE, &other, &transport, &now)), Ok("b1".to_owned()));
    assert_eq!(
        transport.calls(),
        vec!["login bot@example.com pw", "login bot@example.com new"]
    );
}

#[test]
fn unauthorized_request_refreshes_and_retries_once() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    transport.push_refresh(tokens("a2", "r2", 900));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();
    let sent = Mutex::new(Vec::new());

    let result = block_on(auth.send_authorized(BASE, &creds, &transport, &now, |token| {
        sent.lock().push(token.clone());
        async move {
            if token == "a1" {
                Err(TestError::Unauthorized)
            } else {
                Ok(token)
            }
        }
    }));
    assert_eq!(result, Ok("a2".to_owned()));
    assert_eq!(*sent.lock(), vec!["a1", "a2"]);
    assert_eq!(transport.calls(), vec!["login bot@example.com pw", "refresh r1"]);
    assert_eq!(
        auth.status(),
        AuthStatus::SignedIn {
            email: "bot@example.com".to_owned()
        }
    );
}

#[test]
fn second_unauthorized_response_is_final() {
    let auth = Auth::default();
    let transport =
        FakeTransport::with_logins(vec![tokens("a1", "r1", 900), tokens("a3", "r3", 900)]);
    transport.push_refresh(tokens("a2", "r2", 900));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();
    let attempts = Mutex::new(0);

    let result: Result<(), TestError> =
        block_on(auth.send_authorized(BASE, &creds, &transport, &now, |_| {
            *attempts.lock() += 1;
            async { Err(TestError::Unauthorized) }
        }));
    assert_eq!(result, Err(TestError::Unauthorized));
    assert_eq!(*attempts.lock(), 2);
    assert_eq!(auth.status(), AuthStatus::Failed(AuthError::LoginRejected(401)));
}

#[test]
fn unauthorized_static_token_is_not_retried() {
    let auth = Auth::default();
    let transport = FakeTransport::default();
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = Credentials::Token("static".to_owned());
    let attempts = Mutex::new(0);

    let result: Result<(), TestError> =
        block_on(auth.send_authorized(BASE, &creds, &transport, &now, |_| {
            *attempts.lock() += 1;
            async { Err(TestError::Unauthorized) }
        }));
    assert_eq!(result, Err(TestError::Unauthorized));
    assert_eq!(*attempts.lock(), 1);
    assert!(transport.calls().is_empty());
    assert_eq!(auth.status(), AuthStatus::Failed(AuthError::TokenRejected));
}

#[test]
fn auth_failure_is_returned_and_shown() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![Err(TransportError::Status(401))]);
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    let result: Result<(), TestError> =
        block_on(auth.send_authorized(BASE, &creds, &transport, &now, |_| async {
            panic!("no request without a token")
        }));
    assert_eq!(result, Err(TestError::Auth(AuthError::LoginRejected(401))));
    assert_eq!(auth.status(), AuthStatus::Failed(AuthError::LoginRejected(401)));
}

#[test]
fn concurrent_requests_share_one_login() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    let (first, second) = block_on(futures::future::join(
        auth.token(BASE, &creds, &transport, &now),
        auth.token(BASE, &creds, &transport, &now),
    ));
    assert_eq!(first, Ok("a1".to_owned()));
    assert_eq!(second, Ok("a1".to_owned()));
    assert_eq!(transport.calls(), vec!["login bot@example.com pw"]);
}

#[test]
fn stale_invalidation_keeps_the_newer_token() {
    let auth = Auth::default();
    let transport = FakeTransport::with_logins(vec![tokens("a1", "r1", 900)]);
    transport.push_refresh(tokens("a2", "r2", 900));
    let clock = Clock::new();
    let now = clock.now_fn();
    let creds = account();

    block_on(auth.token(BASE, &creds, &transport, &now)).unwrap();
    block_on(auth.invalidate("a1"));
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a2".to_owned()));
    // A request that still held a1 reports its 401 late: a2 stays.
    block_on(auth.invalidate("a1"));
    assert_eq!(block_on(auth.token(BASE, &creds, &transport, &now)), Ok("a2".to_owned()));
    assert_eq!(transport.calls(), vec!["login bot@example.com pw", "refresh r1"]);
}

#[test]
fn debug_output_hides_secrets() {
    let creds = Credentials::Password {
        email: "bot@example.com".to_owned(),
        password: "hunter2".to_owned(),
    };
    let printed = format!("{creds:?}");
    assert!(!printed.contains("hunter2"));
    assert!(printed.contains("bot@example.com"));
    assert_eq!(
        format!("{:?}", Credentials::Token("tok".to_owned())),
        "Token(<redacted>)"
    );
    let response = TokenResponse {
        access_token: Some("access-secret".to_owned()),
        refresh_token: Some("refresh-secret".to_owned()),
        expires_in: Some(900),
        requires_2fa: None,
    };
    let printed = format!("{response:?}");
    assert!(!printed.contains("access-secret"));
    assert!(!printed.contains("refresh-secret"));
}
