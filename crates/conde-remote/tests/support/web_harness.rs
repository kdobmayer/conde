// The web test harness, shared by conde-remote's suite (in-memory store) and conde's suite
// (SQLite and Postgres stores) through `include!`: a fake OpenID provider (discovery, JWKS
// with an RSA key generated here, authorize, token with PKCE and client-secret checks), a
// cookie-jar browser, the `conde login` loopback and the admin API client. No `use` lines:
// the including file imports what these items name.

const TENANT: &str = "2e51b484-b008-4fc0-8f70-555c1317b555";
const CLIENT_ID: &str = "conde-client";
const CLIENT_SECRET: &str = "conde-secret";
const START: i64 = 1_800_000_000;
const ADMIN_KEY: &str = "admin-key-0123456789abcdef0123456789";

/// Live bridge connections by host; revocation must close them.
#[derive(Default)]
struct FakeRegistry {
    open: Mutex<HashSet<String>>,
    closed: Mutex<Vec<String>>,
}

impl FakeRegistry {
    fn connect(&self, host: &str) {
        self.open.lock().unwrap().insert(host.into());
    }
    fn is_open(&self, host: &str) -> bool {
        self.open.lock().unwrap().contains(host)
    }
    fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }
}

impl BridgeRegistry for FakeRegistry {
    fn disconnect_host(&self, host_id: &str) {
        self.open.lock().unwrap().remove(host_id);
        self.closed.lock().unwrap().push(host_id.into());
    }
}


// --- fake OpenID provider -------------------------------------------------------------------

/// How the next id token is spoiled.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spoil {
    None,
    Audience,
    Nonce,
    Expired,
    Signature,
    Issuer,
}

#[derive(Clone)]
struct Person {
    tid: String,
    oid: String,
    upn: String,
    name: String,
}

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
}

struct Idp {
    issuer: String,
    /// (kid, key) signing now; the JWKS publishes only this key.
    key: Mutex<(String, RsaPrivateKey)>,
    person: Mutex<Person>,
    spoil: Mutex<Spoil>,
    grants: Mutex<HashMap<String, Grant>>,
    discoveries: AtomicUsize,
    jwks_reads: AtomicUsize,
    authorizations: AtomicUsize,
}

fn rsa_key(i: usize) -> RsaPrivateKey {
    static KEYS: OnceLock<Vec<RsaPrivateKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        (0..2)
            .map(|_| RsaPrivateKey::new(&mut rand_core06::OsRng, 2048).expect("rsa keygen"))
            .collect()
    })[i]
        .clone()
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

impl Idp {
    fn sign(&self, claims: &Value) -> String {
        let (kid, key) = self.key.lock().unwrap().clone();
        let header = json!({"alg": "RS256", "typ": "JWT", "kid": kid});
        let input = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(claims.to_string().as_bytes())
        );
        let mut sig = SigningKey::<Sha256>::new(key)
            .sign(input.as_bytes())
            .to_vec();
        if *self.spoil.lock().unwrap() == Spoil::Signature {
            sig[10] ^= 0xff;
        }
        format!("{input}.{}", b64(&sig))
    }

    fn rotate(&self) {
        *self.key.lock().unwrap() = ("kid-2".into(), rsa_key(1));
    }
}

async fn discovery(State(idp): State<Arc<Idp>>) -> Response {
    idp.discoveries.fetch_add(1, Ordering::SeqCst);
    let i = &idp.issuer;
    let base = i.trim_end_matches("/v2.0");
    axum::Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{base}/oauth2/v2.0/authorize"),
        "token_endpoint": format!("{base}/oauth2/v2.0/token"),
        "jwks_uri": format!("{base}/discovery/v2.0/keys"),
        "end_session_endpoint": format!("{base}/oauth2/v2.0/logout"),
        "response_types_supported": ["code", "id_token", "code id_token"],
        "subject_types_supported": ["pairwise"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid", "profile", "email", "offline_access"],
        "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
    }))
    .into_response()
}

async fn jwks(State(idp): State<Arc<Idp>>) -> Response {
    idp.jwks_reads.fetch_add(1, Ordering::SeqCst);
    let (kid, key) = idp.key.lock().unwrap().clone();
    let public = key.to_public_key();
    axum::Json(json!({"keys": [{
        "kty": "RSA", "use": "sig", "alg": "RS256", "kid": kid,
        "n": b64(&public.n().to_bytes_be()), "e": b64(&public.e().to_bytes_be()),
    }]}))
    .into_response()
}

async fn authorize(
    State(idp): State<Arc<Idp>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    idp.authorizations.fetch_add(1, Ordering::SeqCst);
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    assert_eq!(get("response_type"), "code");
    assert_eq!(get("client_id"), CLIENT_ID);
    assert_eq!(get("code_challenge_method"), "S256");
    let scopes: HashSet<&str> = q
        .get("scope")
        .map(|s| s.split(' ').collect())
        .unwrap_or_default();
    assert!(
        ["openid", "profile", "email"]
            .iter()
            .all(|s| scopes.contains(s)),
        "{scopes:?}"
    );
    let code = format!("idp-code-{}", rand_hex());
    idp.grants.lock().unwrap().insert(
        code.clone(),
        Grant {
            nonce: get("nonce"),
            challenge: get("code_challenge"),
            redirect_uri: get("redirect_uri"),
        },
    );
    let sep = if get("redirect_uri").contains('?') {
        '&'
    } else {
        '?'
    };
    Redirect::to(&format!(
        "{}{sep}code={code}&state={}",
        get("redirect_uri"),
        get("state")
    ))
    .into_response()
}

async fn token(
    State(idp): State<Arc<Idp>>,
    headers: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let refuse = |m: &str| {
        (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant", "error_description": m})),
        )
            .into_response()
    };
    // client_secret_post only, as the board is configured for Entra.
    let by_post = f.get("client_id").map(String::as_str) == Some(CLIENT_ID)
        && f.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET);
    if headers.contains_key(header::AUTHORIZATION) || !by_post {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    if f.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return refuse("grant_type");
    }
    let Some(grant) = f
        .get("code")
        .and_then(|c| idp.grants.lock().unwrap().remove(c))
    else {
        return refuse("unknown code");
    };
    if f.get("redirect_uri") != Some(&grant.redirect_uri) {
        return refuse("redirect_uri");
    }
    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
    if b64(&sha2::Sha256::digest(verifier.as_bytes())) != grant.challenge {
        return refuse("pkce");
    }
    let spoil = *idp.spoil.lock().unwrap();
    let p = idp.person.lock().unwrap().clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let exp = if spoil == Spoil::Expired {
        now - 600
    } else {
        now + 3600
    };
    let claims = json!({
        "iss": if spoil == Spoil::Issuer { "https://login.microsoftonline.com/other/v2.0".to_string() } else { idp.issuer.clone() },
        "aud": if spoil == Spoil::Audience { "someone-else" } else { CLIENT_ID },
        "sub": format!("pairwise-{}", p.oid),
        "iat": now - 5, "nbf": now - 5, "exp": exp,
        "nonce": if spoil == Spoil::Nonce { "replayed".to_string() } else { grant.nonce },
        "tid": p.tid, "oid": p.oid, "name": p.name, "preferred_username": p.upn,
        "ver": "2.0",
    });
    axum::Json(json!({
        "access_token": "at", "token_type": "Bearer", "expires_in": 3600, "scope": "openid profile email",
        "id_token": idp.sign(&claims),
    }))
    .into_response()
}

async fn start_idp() -> Arc<Idp> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let idp = Arc::new(Idp {
        issuer: format!("http://{addr}/{TENANT}/v2.0"),
        key: Mutex::new(("kid-1".into(), rsa_key(0))),
        person: Mutex::new(Person {
            tid: TENANT.into(),
            oid: "oid-krisztian".into(),
            upn: "Krisztian.Dobmayer@dakai.example".into(),
            name: "Krisztián Dobmayer".into(),
        }),
        spoil: Mutex::new(Spoil::None),
        grants: Mutex::new(HashMap::new()),
        discoveries: AtomicUsize::new(0),
        jwks_reads: AtomicUsize::new(0),
        authorizations: AtomicUsize::new(0),
    });
    let t = TENANT;
    let app = Router::new()
        .route(
            &format!("/{t}/v2.0/.well-known/openid-configuration"),
            get(discovery),
        )
        .route(&format!("/{t}/discovery/v2.0/keys"), get(jwks))
        .route(&format!("/{t}/oauth2/v2.0/authorize"), get(authorize))
        .route(&format!("/{t}/oauth2/v2.0/token"), post(token))
        .route(
            &format!("/{t}/oauth2/v2.0/logout"),
            get(|| async { "signed out of the provider" }),
        )
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    idp
}

fn rand_hex() -> String {
    format!("{:032x}", rand::random::<u128>())
}

// --- harness ----------------------------------------------------------------------------------

struct Env<S> {
    base: String,
    idp: Arc<Idp>,
    store: Arc<S>,
    registry: Arc<FakeRegistry>,
    clock: Arc<AtomicI64>,
}

impl<S: WebStore + 'static> Env<S> {
    /// The sign-in and keys routes plus the admin API over `store`, against a fresh fake
    /// provider, with a test clock starting at [`START`].
    async fn with(store: Arc<S>) -> Env<S> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let idp = start_idp().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut config = WebConfig::entra(
            TENANT,
            CLIENT_ID,
            CLIENT_SECRET,
            &format!("{base}/auth/callback"),
        )
        .unwrap();
        config.issuer = idp.issuer.clone();
        let registry = Arc::new(FakeRegistry::default());
        let clock = Arc::new(AtomicI64::new(START));
        let c = clock.clone();
        let state = WebState::with_clock(
            config,
            store.clone(),
            registry.clone(),
            Arc::new(move || c.load(Ordering::SeqCst)),
        );
        let admin = web::AdminState::with_clock(
            store.clone(),
            registry.clone(),
            ADMIN_KEY,
            Some(TENANT.into()),
            Arc::new(|| START),
        )
        .unwrap();
        let app = web::router(state).merge(web::admin_router(admin));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Env {
            base,
            idp,
            store,
            registry,
            clock,
        }
    }

}

impl<S> Env<S> {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn advance(&self, secs: i64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }
}

/// A browser: one cookie jar, a client that follows redirects and one that does not.
struct Browser {
    jar: Arc<reqwest::cookie::Jar>,
    follow: reqwest::Client,
    step: reqwest::Client,
}

impl Browser {
    fn new() -> Browser {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let build = |policy| {
            reqwest::Client::builder()
                .cookie_provider(jar.clone())
                .redirect(policy)
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap()
        };
        Browser {
            follow: build(reqwest::redirect::Policy::limited(10)),
            step: build(reqwest::redirect::Policy::none()),
            jar,
        }
    }

    async fn get(&self, url: &str) -> (StatusCode, String, String) {
        let r = self.follow.get(url).send().await.unwrap();
        let (status, at) = (r.status(), r.url().to_string());
        (status, at, r.text().await.unwrap())
    }

    /// Signs in through the fake provider and returns the keys page.
    async fn sign_in<S>(&self, env: &Env<S>) -> String {
        let (status, at, body) = self.get(&env.url("/keys")).await;
        assert_eq!(status, StatusCode::OK, "{at}: {body}");
        assert_eq!(at, env.url("/keys"));
        body
    }

    /// Creates a key on the keys page: POST, then its one display (POST-redirect-GET).
    async fn create_key<S>(&self, env: &Env<S>, csrf: &str, label: &str) -> String {
        let r = self
            .post_form(&env.url("/keys"), &[("csrf", csrf), ("label", label)])
            .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(r.headers()[header::LOCATION], "/keys/created");
        let (status, at, page) = self.get(&env.url("/keys/created")).await;
        assert_eq!((status, at), (StatusCode::OK, env.url("/keys/created")));
        page
    }

    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> reqwest::Response {
        self.step.post(url).form(form).send().await.unwrap()
    }

    /// Status and Location of one GET without following redirects.
    async fn peek(&self, url: &str) -> (StatusCode, String) {
        let r = self.step.get(url).send().await.unwrap();
        let loc = r
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (r.status(), loc)
    }
}

fn csrf_of(page: &str) -> String {
    let at = page.find("name=\"csrf\" value=\"").expect("a csrf field") + 19;
    page[at..at + 64].to_string()
}

fn key_of(page: &str) -> String {
    let at = page.find("<pre id=\"key\">").expect("the key") + 14;
    page[at..].split('<').next().unwrap().trim().to_string()
}


/// The CLI's loopback listener: records each `/done` query.
async fn loopback() -> (u16, Arc<Mutex<Vec<HashMap<String, String>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let app = Router::new().route(
        "/done",
        get(move |Query(q): Query<HashMap<String, String>>| {
            let s = s.clone();
            async move {
                s.lock().unwrap().push(q);
                "signed in"
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, seen)
}

struct CliRun {
    state: String,
    verifier: String,
    challenge: String,
}

fn cli_run() -> CliRun {
    let verifier = b64(&rand::random::<[u8; 32]>());
    let challenge = b64(&sha2::Sha256::digest(verifier.as_bytes()));
    CliRun {
        state: rand_hex(),
        verifier,
        challenge,
    }
}

async fn redeem<S>(env: &Env<S>, body: Value) -> (StatusCode, Value) {
    let r = reqwest::Client::new()
        .post(env.url("/auth/cli/redeem"))
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status(), r.json().await.unwrap())
}

/// Runs the browser leg; returns the one-time code the loopback received.
async fn browser_leg<S>(env: &Env<S>, b: &Browser, run: &CliRun) -> String {
    let (port, seen) = loopback().await;
    let url = env.url(&format!(
        "/auth/cli?port={port}&state={}&challenge={}",
        run.state, run.challenge
    ));
    let (status, at, body) = b.get(&url).await;
    assert_eq!(status, StatusCode::OK, "{at}: {body}");
    assert!(
        at.starts_with(&format!("http://127.0.0.1:{port}/done?code=")),
        "{at}"
    );
    let q = seen.lock().unwrap().pop().expect("the loopback was called");
    assert_eq!(q["state"], run.state);
    q["code"].clone()
}


async fn admin<S>(
    env: &Env<S>,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let client = reqwest::Client::new();
    let mut req = match body {
        Some(b) => client.post(env.url(path)).json(&b),
        None => client.get(env.url(path)),
    };
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let r = req.send().await.unwrap();
    let status = r.status();
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    (status, r.json().await.unwrap_or(Value::Null))
}

