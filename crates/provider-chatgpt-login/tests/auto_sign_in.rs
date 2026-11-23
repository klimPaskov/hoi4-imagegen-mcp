//! The sign-in the tool starts by itself after the server revoked the sign-in (decision D-143): the page opens once per
//! revocation across processes, the wait ends after its timeout with a clear status, a sign-in that lands clears the
//! state, and nothing opens the page twice for one revocation.
//!
//! SYNTHETIC: the browser is a local socket thread and the token endpoint is a fixture. No network is used, and the
//! credentials are made up. The 401 body is the documented shape from the incident report, not a recording.

#![allow(clippy::result_large_err)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use imagegen_core::config::MapEnv;
use imagegen_core::hashing::Clock;
use imagegen_providers::transport::{FixtureTransport, Method};
use imagegen_providers::types::{CallContext, ImageProvider, ImageRequest};
use provider_chatgpt_login::cli::AutoSignIn;
use provider_chatgpt_login::route::{self, concurrency_setting, Route};
use provider_chatgpt_login::store::{CredentialStore, Credentials};
use provider_chatgpt_login::ChatGptLoginProvider;

const BACKEND_PATH: &str = "/backend-api/codex/responses";
const REVOKED_BODY: &str =
    r#"{"error":{"message":"Encountered invalidated oauth token for user","type":"invalid_request_error","code":"token_revoked"}}"#;
const TOKENS: &[u8] = br#"{"access_token":"access-after-sign-in","refresh_token":"refresh-after-sign-in","expires_in":3600}"#;
const NOW: i64 = 1_793_561_534;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_secs(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn work_dir() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(".work");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new().prefix("as").tempdir_in(base).unwrap()
}

fn request() -> ImageRequest {
    ImageRequest {
        model: provider_chatgpt_login::MODEL.into(),
        prompt: "a fortified concrete wall".into(),
        negative: vec![],
        width: 100,
        height: 88,
        transparent: true,
        quality: None,
        refs: vec![],
        edit: None,
        idempotency_key: None,
        seed: None,
    }
}

/// Counts the pages the tool opened and hands each address to a "browser".
struct Browser {
    opened: Arc<AtomicUsize>,
    urls: Mutex<std::sync::mpsc::Receiver<String>>,
    tx: Sender<String>,
}

impl Browser {
    fn new() -> Self {
        let (tx, rx) = channel();
        Browser { opened: Arc::new(AtomicUsize::new(0)), urls: Mutex::new(rx), tx }
    }

    fn auto(&self, timeout: Duration) -> AutoSignIn {
        let (opened, tx) = (self.opened.clone(), self.tx.clone());
        AutoSignIn {
            open: Arc::new(move |url| {
                opened.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(url.to_string());
            }),
            port: 0,
            timeout,
            consent_file: None,
        }
    }

    fn pages(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    /// The user finishes the page: the browser calls the local callback with the code and the state.
    fn finish_page(&self) {
        let url = self.urls.lock().unwrap().recv_timeout(Duration::from_secs(10)).expect("the page was opened");
        let get = |name: &str| url.split(&['?', '&'][..]).find_map(|kv| kv.strip_prefix(&format!("{name}="))).map(str::to_string).unwrap();
        let state = get("state");
        let redirect = get("redirect_uri");
        let port: u16 = redirect.rsplit("%3A").next().unwrap().split('%').next().unwrap().parse().unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET /auth/callback?code=synthetic-code&state={state} HTTP/1.1\r\n\r\n").unwrap();
        let mut r = String::new();
        s.read_to_string(&mut r).ok();
    }
}

struct Machine {
    dir: tempfile::TempDir,
    fixture: Arc<FixtureTransport>,
    clock: Arc<TestClock>,
}

impl Machine {
    fn new() -> Self {
        let dir = work_dir();
        CredentialStore::at(dir.path().join("auth.json"))
            .save(&Credentials {
                access: "access-before".into(),
                refresh: Some("refresh-before".into()),
                account_id: Some("acct-synthetic".into()),
                expires_ms: None,
                last_refresh: "t".into(),
            })
            .unwrap();
        let fixture = Arc::new(FixtureTransport::new());
        fixture.respond(Method::Post, "/oauth/token", 200, &[], TOKENS.to_vec());
        Machine { dir, fixture, clock: Arc::new(TestClock(AtomicI64::new(NOW))) }
    }

    fn store(&self) -> CredentialStore {
        CredentialStore::at(self.dir.path().join("auth.json"))
    }

    fn route(&self) -> Route {
        Route::new(self.store().state_dir())
    }

    /// One more process of the machine, with its own provider.
    fn process(&self, auto: Option<AutoSignIn>) -> ChatGptLoginProvider {
        ChatGptLoginProvider::new(self.store(), self.fixture.clone(), self.clock.clone()).with_auto_sign_in(auto)
    }

    fn server_signs_out(&self) {
        self.fixture.clear_route(Method::Post, BACKEND_PATH).respond(Method::Post, BACKEND_PATH, 401, &[], REVOKED_BODY.as_bytes().to_vec());
    }
}

/// Never waits: the pacing gap of the route is not what these tests are about.
struct NoSleep;
impl imagegen_providers::types::Sleeper for NoSleep {
    fn sleep(&self, _d: Duration) {}
}

fn generate(p: &ChatGptLoginProvider) -> String {
    let ctx = CallContext { cancel: imagegen_providers::types::CancelToken::new(), sleeper: Arc::new(NoSleep), jobs: None };
    p.generate(&request(), &ctx).unwrap_err().message
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_page_opens_once_per_revocation_even_when_many_processes_claim_it() {
    let m = Machine::new();
    let browser = Browser::new();
    let one = m.process(Some(browser.auto(Duration::from_secs(30))));
    m.server_signs_out();
    // the first process to see the 401 records the revocation and opens the page
    let e = generate(&one);
    assert!(e.starts_with("signed out by the server"), "{e}");
    assert_eq!(browser.pages(), 1);
    let st = m.route().state();
    let pending = st.sign_in.as_ref().expect("the sign-in is recorded");
    assert_eq!(
        (pending.owner_pid, pending.outcome.as_deref(), Some(pending.revoked_epoch)),
        (std::process::id(), None, st.revoked.as_ref().map(|r| r.at_epoch))
    );
    // other processes see the revocation and neither call the backend nor open another page
    let other = m.process(Some(browser.auto(Duration::from_secs(30))));
    assert!(generate(&other).starts_with("signed out by the server"));
    assert_eq!(browser.pages(), 1, "no second page for the same revocation");
    // twelve processes claiming at the same moment: nobody wins
    let wins: usize = (0..12)
        .map(|_| {
            let route = m.route();
            std::thread::spawn(move || usize::from(route.claim_sign_in(NOW, 1, 3600).unwrap()))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .sum();
    assert_eq!(wins, 0);
    // and before any claim exactly one of twelve wins
    let fresh = work_dir();
    let route = Route::new(Some(fresh.path().to_path_buf()));
    route.acquire(&imagegen_providers::types::CancelToken::new(), &imagegen_providers::types::RealSleeper).unwrap().record_revoked(
        NOW,
        401,
        b"",
        "generation",
    );
    let wins: usize = (0..12)
        .map(|n| {
            let route = Route::new(Some(fresh.path().to_path_buf()));
            std::thread::spawn(move || usize::from(route.claim_sign_in(NOW + 1, n, 3600).unwrap()))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .sum();
    assert_eq!(wins, 1, "exactly one process may open the page");
    // nothing opens without a revocation
    assert!(!Route::new(Some(work_dir().path().to_path_buf())).claim_sign_in(NOW, 1, 3600).unwrap());
    browser.finish_page(); // let the first listener end
    wait_for("the sign-in", || m.route().state().sign_in.is_none());
}

#[test]
fn status_waits_for_the_sign_in_and_the_landing_clears_everything_and_a_new_revocation_opens_a_new_page() {
    let m = Machine::new();
    let browser = Browser::new();
    let p = m.process(Some(browser.auto(Duration::from_secs(3600))));
    m.server_signs_out();
    generate(&p);
    let setting = concurrency_setting(&MapEnv::from_pairs(&[]), None, None);
    let r = m.route().report(&setting, NOW + 60);
    let s = &r["sign_in"];
    assert_eq!(s["state"], "waiting_for_sign_in", "{s}");
    assert_eq!((s["owner_alive"].as_bool(), s["remaining_secs"].as_i64()), (Some(true), Some(3600 - 60)), "{s}");
    assert_eq!((s["started_at"].as_str(), s["revoked_at"].as_str()), (Some("2026-11-01T19:32:14Z"), Some("2026-11-01T19:32:14Z")));
    assert!(s["message"].as_str().unwrap().contains("waits for you to finish it until 2026-11-01T20:32:14Z"), "{s}");
    assert_eq!(r["revoked"], true);
    // the doctor's text names the wait
    let text = provider_chatgpt_login::cli::status_text_at(&m.store(), NOW).unwrap();
    assert!(text.contains("waiting for you to sign in") && text.contains("2026-11-01T20:32:14Z"), "{text}");

    // the user finishes the page: the tokens are stored, the revocation and the wait are gone, and the route carries on
    browser.finish_page();
    wait_for("the sign-in to land", || m.route().state().sign_in.is_none() && m.route().state().revoked.is_none());
    assert_eq!(m.store().load().unwrap().unwrap().access, "access-after-sign-in");
    assert!(m.route().report(&setting, NOW + 120)["sign_in"].is_null());
    wait_for("the listener to end", || !m.route().sign_in_owner_alive());
    // the learned limit of the sign-out stays (decision D-141)
    assert_eq!(m.route().state().learned_limit(), Some(9));
    // a later revocation is a new one: a new page opens, once
    m.server_signs_out();
    generate(&p);
    assert_eq!(browser.pages(), 2);
    browser.finish_page();
    wait_for("the second sign-in", || m.route().state().sign_in.is_none() && m.route().state().revoked.is_none());
    assert_eq!(m.route().state().learned_limit(), Some(8));
}

#[test]
fn a_sign_in_that_is_not_finished_ends_with_a_clear_status_and_never_opens_again() {
    let m = Machine::new();
    let browser = Browser::new();
    // the wait is one second here: the production wait is an hour (checked below)
    let p = m.process(Some(browser.auto(Duration::from_secs(1))));
    m.server_signs_out();
    generate(&p);
    assert_eq!(browser.pages(), 1);
    wait_for("the wait to end", || m.route().state().sign_in.as_ref().is_some_and(|s| s.outcome.is_some()));
    assert_eq!(m.route().state().sign_in.unwrap().outcome.as_deref(), Some("timed_out"));
    let setting = concurrency_setting(&MapEnv::from_pairs(&[]), None, None);
    let s = m.route().report(&setting, NOW + 4000)["sign_in"].clone();
    assert_eq!(s["state"], "timed_out", "{s}");
    assert!(
        s["message"].as_str().unwrap().contains("was not finished within an hour") && s["message"].as_str().unwrap().contains("hoi4-imagegen login"),
        "{s}"
    );
    // the revocation stays, the calls still stop, and the page is not opened again, however often a call is made
    for _ in 0..3 {
        assert!(generate(&p).starts_with("signed out by the server"));
    }
    assert_eq!(browser.pages(), 1, "the page is never opened in a loop");
    assert!(m.route().state().revoked.is_some());
    // the sign-in the user starts himself still works and clears it
    let text = provider_chatgpt_login::cli::status_text_at(&m.store(), NOW).unwrap();
    assert!(text.contains("hoi4-imagegen login"), "{text}");
}

#[test]
fn a_listener_that_is_gone_is_reported_and_the_production_setting_is_an_hour_on_the_registered_port() {
    let m = Machine::new();
    let browser = Browser::new();
    // a process claims the sign-in and ends before anything listens: nobody holds the listener lock
    let route = m.route();
    route.acquire(&imagegen_providers::types::CancelToken::new(), &imagegen_providers::types::RealSleeper).unwrap().record_revoked(
        NOW,
        401,
        b"",
        "generation",
    );
    assert!(route.claim_sign_in(NOW, 4242, 3600).unwrap());
    let setting = concurrency_setting(&MapEnv::from_pairs(&[]), None, None);
    let s = route.report(&setting, NOW + 10)["sign_in"].clone();
    assert_eq!((s["state"].as_str(), s["owner_alive"].as_bool()), (Some("listener_stopped"), Some(false)), "{s}");
    assert!(s["message"].as_str().unwrap().contains("hoi4-imagegen login"), "{s}");
    assert_eq!(browser.pages(), 0);
    // production: an hour, the registered port, and not at all when no browser is wanted
    let auto = AutoSignIn::for_environment(&MapEnv::from_pairs(&[("APPDATA", "x"), ("HOME", "x")])).expect("on by default");
    assert_eq!((auto.timeout, auto.port), (Duration::from_secs(3600), provider_chatgpt_login::CALLBACK_PORT));
    assert_eq!(route::AUTO_SIGN_IN_SECS, 3600);
    assert!(AutoSignIn::for_environment(&MapEnv::from_pairs(&[("HOI4_IMAGEGEN_NO_BROWSER", "1")])).is_none());
    assert!(AutoSignIn::for_environment(&MapEnv::from_pairs(&[("HOI4_IMAGEGEN_NO_AUTO_SIGN_IN", "1")])).is_none());
}

#[test]
fn a_process_without_the_setting_never_opens_a_page() {
    let m = Machine::new();
    let p = m.process(None);
    m.server_signs_out();
    assert!(generate(&p).starts_with("signed out by the server"));
    assert!(m.route().state().sign_in.is_none(), "nothing was claimed");
}
