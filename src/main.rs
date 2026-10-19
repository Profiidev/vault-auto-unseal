use std::{
  ffi::c_int,
  io::Write,
  sync::mpsc::{self, RecvTimeoutError},
  thread::spawn,
  time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use signal_hook::{consts::TERM_SIGNALS, iterator::Signals};
use ureq::{
  Agent,
  http::StatusCode,
  tls::{Certificate, RootCerts, TlsConfig},
};

#[cfg(not(test))]
const INTERVAL: Duration = Duration::from_secs(15);
#[cfg(test)]
const INTERVAL: Duration = Duration::from_millis(50);

fn main() {
  std::process::exit(run(
    TERM_SIGNALS,
    |k| std::env::var(k).ok(),
    &mut std::io::stdout(),
  ));
}

/// Unseal loop. Runs until one of `signals` arrives, returns the process exit code.
fn run(signals: &[c_int], get: impl Fn(&str) -> Option<String>, out: &mut impl Write) -> i32 {
  let mut signals = match Signals::new(signals) {
    Ok(signals) => signals,
    Err(err) => {
      let _ = writeln!(out, "Failed to register signal: {err}");
      return 1;
    }
  };
  let (tx, rx) = mpsc::channel();
  spawn(move || {
    let _ = tx.send(signals.forever().next());
  });

  loop {
    let _ = match Config::from_lookup(&get).and_then(|c| unseal(&c)) {
      Ok(true) => writeln!(out, "Unlock successful"),
      Ok(false) => Ok(()),
      Err(err) => writeln!(out, "Unseal error: {err}"),
    };
    if rx.recv_timeout(INTERVAL) != Err(RecvTimeoutError::Timeout) {
      let _ = writeln!(out, "Got exit signal");
      return 0;
    }
  }
}

#[derive(Debug)]
struct Config {
  vault_url: String,
  ca_cert: Option<String>,
  keys: [String; 3],
}

impl Config {
  fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
    // empty values count as unset
    let get = |k: &str| get(k).filter(|v| !v.is_empty());
    let req = |k: &str| get(k).with_context(|| format!("missing env var {k}"));
    Ok(Self {
      vault_url: req("VAULT_URL")?.trim_end_matches('/').to_string(),
      ca_cert: get("CA_CERT"),
      keys: [req("KEY_1")?, req("KEY_2")?, req("KEY_3")?],
    })
  }
}

fn agent(ca_cert: Option<&str>) -> Result<Agent> {
  let mut config = Agent::config_builder().http_status_as_error(false);
  if let Some(cert) = ca_cert {
    let cert = Certificate::from_pem(cert.as_bytes())?;
    let tls = TlsConfig::builder()
      .root_certs(RootCerts::new_with_certs(&[cert]))
      .build();
    config = config.tls_config(tls);
  }
  Ok(Agent::new_with_config(config.build()))
}

/// Returns `Ok(true)` if vault was sealed and is now unsealed, `Ok(false)` if nothing to do.
fn unseal(config: &Config) -> Result<bool> {
  let agent = agent(config.ca_cert.as_deref())?;
  let url = &config.vault_url;

  let res = agent.get(format!("{url}/v1/sys/health")).call()?;
  if res.status() != StatusCode::SERVICE_UNAVAILABLE {
    return Ok(false);
  }

  let mut sealed = true;
  for key in &config.keys {
    let mut res = agent
      .post(format!("{url}/v1/sys/unseal"))
      .send_json(&UnsealReq { key })?;
    if !res.status().is_success() {
      bail!(
        "unseal request failed with {}: {}",
        res.status(),
        res.body_mut().read_to_string().unwrap_or_default()
      );
    }
    sealed = res.body_mut().read_json::<UnsealRes>()?.sealed;
  }

  if sealed {
    bail!("vault still sealed after submitting all keys");
  }
  Ok(true)
}

#[derive(Serialize)]
struct UnsealReq<'a> {
  key: &'a str,
}

#[derive(Deserialize)]
struct UnsealRes {
  sealed: bool,
}

#[cfg(test)]
mod tests {
  use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread::{scope, sleep},
  };

  use signal_hook::{
    consts::{SIGALRM, SIGUSR1, SIGUSR2},
    low_level::raise,
  };

  use super::*;

  const CA: &str = include_str!("testdata/ca.pem");
  const OTHER_CA: &str = include_str!("testdata/other-ca.pem");
  const SEALED: &str = r#"{"sealed":true}"#;
  const UNSEALED: &str = r#"{"sealed":false}"#;

  fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + Sync {
    let map: HashMap<String, String> = pairs
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect();
    move |k| map.get(k).cloned()
  }

  fn full_env(url: &str) -> Vec<(&str, &str)> {
    vec![
      ("VAULT_URL", url),
      ("KEY_1", "a"),
      ("KEY_2", "b"),
      ("KEY_3", "c"),
    ]
  }

  fn config(url: &str, keys: [&str; 3]) -> Config {
    Config {
      vault_url: url.into(),
      ca_cert: None,
      keys: keys.map(String::from),
    }
  }

  /// Scripted HTTP server: the n-th connection gets the n-th response, the last one repeats forever.
  /// Status 0 closes the connection without answering. Records "METHOD /path body" per request.
  fn fake_vault(responses: &[(u16, &'static str)]) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let (responses, log2) = (responses.to_vec(), log.clone());
    spawn(move || {
      for (i, stream) in listener.incoming().enumerate() {
        let mut reader = BufReader::new(stream.unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request = line.rsplit_once(' ').unwrap().0.to_string();
        let mut len = 0;
        loop {
          let mut header = String::new();
          reader.read_line(&mut header).unwrap();
          if header == "\r\n" {
            break;
          }
          if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap();
          }
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        // normalize JSON formatting
        let body = serde_json::from_slice::<serde_json::Value>(&body)
          .map(|v| v.to_string())
          .unwrap_or_default();
        log2
          .lock()
          .unwrap()
          .push(format!("{request} {body}").trim_end().to_string());

        let (status, body) = responses[i.min(responses.len() - 1)];
        if status != 0 {
          write!(
            reader.get_mut(),
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
          )
          .unwrap();
        }
      }
    });
    (url, log)
  }

  fn log_of(log: &Mutex<Vec<String>>) -> Vec<String> {
    log.lock().unwrap().clone()
  }

  /// Runs `run` in a thread, calls `wait` meanwhile, then raises `signal` and returns (exit code, output).
  fn run_until(
    signal: c_int,
    get: impl Fn(&str) -> Option<String> + Sync,
    wait: impl FnOnce(),
  ) -> (i32, String) {
    scope(|s| {
      let handle = s.spawn(|| {
        let mut out = Vec::new();
        let code = run(&[signal], &get, &mut out);
        (code, String::from_utf8(out).unwrap())
      });
      // absorbs signals raised before `run` registers its handler (default action would kill us)
      let _guard = Signals::new([signal]).unwrap();
      wait();
      while !handle.is_finished() {
        raise(signal).unwrap();
        sleep(Duration::from_millis(5));
      }
      handle.join().unwrap()
    })
  }

  // --- Config ---

  #[test]
  fn config_reads_all_vars() {
    let c = Config::from_lookup(env(&full_env("http://vault:8200"))).unwrap();
    assert_eq!(c.vault_url, "http://vault:8200");
    assert_eq!(c.keys, ["a", "b", "c"]);
    assert!(c.ca_cert.is_none());

    let mut with_ca = full_env("http://vault:8200");
    with_ca.push(("CA_CERT", "pem"));
    let c = Config::from_lookup(env(&with_ca)).unwrap();
    assert_eq!(c.ca_cert.as_deref(), Some("pem"));
  }

  #[test]
  fn config_missing_or_empty_var_errors() {
    for var in ["VAULT_URL", "KEY_1", "KEY_2", "KEY_3"] {
      let missing: Vec<_> = full_env("http://v")
        .into_iter()
        .filter(|(k, _)| *k != var)
        .collect();
      let err = Config::from_lookup(env(&missing)).unwrap_err();
      assert_eq!(err.to_string(), format!("missing env var {var}"));

      let mut empty = missing.clone();
      empty.push((var, ""));
      let err = Config::from_lookup(env(&empty)).unwrap_err();
      assert_eq!(err.to_string(), format!("missing env var {var}"));
    }
  }

  #[test]
  fn config_empty_ca_cert_is_none() {
    let mut pairs = full_env("http://v");
    pairs.push(("CA_CERT", ""));
    assert!(Config::from_lookup(env(&pairs)).unwrap().ca_cert.is_none());
  }

  #[test]
  fn config_trims_trailing_slashes() {
    for url in ["http://v:8200/", "http://v:8200///"] {
      let c = Config::from_lookup(env(&full_env(url))).unwrap();
      assert_eq!(c.vault_url, "http://v:8200");
    }
  }

  // --- agent ---

  #[test]
  fn agent_accepts_valid_ca() {
    assert!(agent(Some(CA)).is_ok());
    assert!(agent(None).is_ok());
  }

  #[test]
  fn agent_rejects_invalid_ca() {
    assert!(agent(Some("not a pem")).is_err());
  }

  // --- unseal against fake server ---

  #[test]
  fn not_sealed_status_is_noop() {
    // active, standby, DR secondary, perf standby, uninitialized
    for status in [200, 429, 472, 473, 501] {
      let (url, log) = fake_vault(&[(status, "")]);
      assert!(!unseal(&config(&url, ["a", "b", "c"])).unwrap(), "{status}");
      assert_eq!(log_of(&log), ["GET /v1/sys/health"], "{status}");
    }
  }

  #[test]
  fn sealed_posts_all_keys_in_order() {
    let (url, log) = fake_vault(&[(503, ""), (200, SEALED), (200, SEALED), (200, UNSEALED)]);
    assert!(unseal(&config(&url, ["a", "b", "c"])).unwrap());
    assert_eq!(
      log_of(&log),
      [
        "GET /v1/sys/health",
        r#"POST /v1/sys/unseal {"key":"a"}"#,
        r#"POST /v1/sys/unseal {"key":"b"}"#,
        r#"POST /v1/sys/unseal {"key":"c"}"#,
      ]
    );
  }

  #[test]
  fn still_sealed_after_all_keys_errors() {
    let (url, log) = fake_vault(&[(503, ""), (200, SEALED)]);
    let err = unseal(&config(&url, ["a", "b", "c"])).unwrap_err();
    assert_eq!(
      err.to_string(),
      "vault still sealed after submitting all keys"
    );
    assert_eq!(log_of(&log).len(), 4);
  }

  #[test]
  fn unseal_error_status_stops_and_reports_body() {
    let (url, log) = fake_vault(&[(503, ""), (500, "boom")]);
    let err = unseal(&config(&url, ["a", "b", "c"])).unwrap_err();
    assert_eq!(
      err.to_string(),
      "unseal request failed with 500 Internal Server Error: boom"
    );
    // remaining keys not sent
    assert_eq!(log_of(&log).len(), 2);
  }

  #[test]
  fn unseal_invalid_response_body_errors() {
    for body in ["not json", "{}", r#"{"sealed":"no"}"#] {
      let (url, log) = fake_vault(&[(503, ""), (200, body)]);
      assert!(unseal(&config(&url, ["a", "b", "c"])).is_err(), "{body}");
      assert_eq!(log_of(&log).len(), 2, "{body}");
    }
  }

  #[test]
  fn connection_closed_errors() {
    let (url, _) = fake_vault(&[(0, "")]);
    assert!(unseal(&config(&url, ["a", "b", "c"])).is_err());

    let (url, log) = fake_vault(&[(503, ""), (0, "")]);
    assert!(unseal(&config(&url, ["a", "b", "c"])).is_err());
    assert_eq!(log_of(&log).len(), 2);
  }

  #[test]
  fn connection_refused_errors() {
    let port = TcpListener::bind("127.0.0.1:0")
      .unwrap()
      .local_addr()
      .unwrap()
      .port();
    assert!(
      unseal(&config(
        &format!("http://127.0.0.1:{port}"),
        ["a", "b", "c"]
      ))
      .is_err()
    );
  }

  #[test]
  fn invalid_url_errors() {
    assert!(unseal(&config("not a url", ["a", "b", "c"])).is_err());
  }

  #[test]
  fn invalid_ca_in_config_errors_before_request() {
    let (url, log) = fake_vault(&[(503, "")]);
    let mut c = config(&url, ["a", "b", "c"]);
    c.ca_cert = Some("not a pem".into());
    assert!(unseal(&c).is_err());
    assert!(log_of(&log).is_empty());
  }

  // --- run loop ---

  #[test]
  fn run_invalid_signal_fails() {
    let mut out = Vec::new();
    assert_eq!(run(&[0], env(&[]), &mut out), 1);
    assert!(
      String::from_utf8(out)
        .unwrap()
        .starts_with("Failed to register signal: ")
    );
  }

  #[test]
  fn run_retries_on_error_until_signal() {
    let (code, out) = run_until(SIGUSR1, env(&[]), || sleep(INTERVAL * 5));
    assert_eq!(code, 0);
    let lines: Vec<_> = out.lines().collect();
    let (last, errors) = lines.split_last().unwrap();
    assert_eq!(*last, "Got exit signal");
    assert!(errors.len() >= 3, "{out}");
    assert!(
      errors
        .iter()
        .all(|l| *l == "Unseal error: missing env var VAULT_URL"),
      "{out}"
    );
  }

  #[test]
  fn run_unseals_once_then_idles() {
    let (url, log) = fake_vault(&[
      (503, ""),
      (200, SEALED),
      (200, SEALED),
      (200, UNSEALED),
      (200, ""),
    ]);
    let (code, out) = run_until(SIGUSR2, env(&full_env(&url)), || {
      sleep(INTERVAL * 5);
    });
    assert_eq!(code, 0);
    assert_eq!(out, "Unlock successful\nGot exit signal\n");
    // later iterations only check health
    let log = log_of(&log);
    assert!(log.len() >= 6, "{log:?}");
    assert!(
      log[4..].iter().all(|l| l == "GET /v1/sys/health"),
      "{log:?}"
    );
  }

  #[test]
  fn run_exits_immediately_on_signal() {
    // signal during the wait must not wait for the interval to elapse
    let start = std::time::Instant::now();
    let (code, out) = run_until(SIGALRM, env(&[]), || ());
    assert_eq!(code, 0);
    assert!(out.ends_with("Got exit signal\n"), "{out}");
    assert!(start.elapsed() < INTERVAL * 4);
  }

  /// Real vault containers.
  mod vault {
    use serde_json::{Value, json};
    use signal_hook::consts::SIGHUP;
    use testcontainers::{
      Container, GenericImage, ImageExt,
      core::{IntoContainerPort, WaitFor},
      runners::SyncRunner,
    };

    use super::*;

    struct TestVault {
      _container: Container<GenericImage>,
      url: String,
      agent: Agent,
    }

    /// Real (non-dev) vault with in-memory storage, starts sealed and uninitialized.
    fn start_vault(tls: bool) -> TestVault {
      let listener = if tls {
        r#"{"address":"0.0.0.0:8200","tls_cert_file":"/vault/tls/server.pem","tls_key_file":"/vault/tls/server-key.pem"}"#
      } else {
        r#"{"address":"0.0.0.0:8200","tls_disable":true}"#
      };
      let container = GenericImage::new("hashicorp/vault", "2.2")
        .with_exposed_port(8200.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Vault server started!"))
        .with_env_var("SKIP_SETCAP", "true")
        .with_env_var(
          "VAULT_LOCAL_CONFIG",
          format!(
            r#"{{"storage":{{"inmem":{{}}}},"listener":{{"tcp":{listener}}},"disable_mlock":true}}"#
          ),
        )
        .with_copy_to(
          "/vault/tls/server.pem",
          include_bytes!("testdata/server.pem").to_vec(),
        )
        .with_copy_to(
          "/vault/tls/server-key.pem",
          include_bytes!("testdata/server-key.pem").to_vec(),
        )
        .with_cmd(["server"])
        .start()
        .expect("failed to start vault container");
      let scheme = if tls { "https" } else { "http" };
      let url = format!(
        "{scheme}://{}:{}",
        container.get_host().unwrap(),
        container.get_host_port_ipv4(8200).unwrap()
      );
      TestVault {
        _container: container,
        url,
        agent: agent(Some(CA)).unwrap(),
      }
    }

    impl TestVault {
      /// Initializes vault, returns the unseal keys.
      fn init(&self, shares: u8, threshold: u8) -> Vec<String> {
        let res: Value = self
          .agent
          .put(format!("{}/v1/sys/init", self.url))
          .send_json(json!({ "secret_shares": shares, "secret_threshold": threshold }))
          .unwrap()
          .body_mut()
          .read_json()
          .unwrap();
        serde_json::from_value(res["keys"].clone()).unwrap()
      }

      fn is_sealed(&self) -> bool {
        let res: Value = self
          .agent
          .get(format!("{}/v1/sys/seal-status", self.url))
          .call()
          .unwrap()
          .body_mut()
          .read_json()
          .unwrap();
        res["sealed"].as_bool().unwrap()
      }

      fn config(&self, keys: [&str; 3]) -> Config {
        config(&self.url, keys)
      }
    }

    #[test]
    fn unseals_sealed_vault_then_noop() {
      let v = start_vault(false);
      let keys = v.init(5, 3);
      assert!(v.is_sealed());

      let c = v.config([&keys[0], &keys[2], &keys[4]]);
      assert!(unseal(&c).unwrap());
      assert!(!v.is_sealed());

      // already unsealed: nothing to do
      assert!(!unseal(&c).unwrap());
    }

    #[test]
    fn threshold_below_key_count_unseals() {
      // third key hits an already unsealed vault
      let v = start_vault(false);
      let keys = v.init(5, 2);
      assert!(unseal(&v.config([&keys[0], &keys[1], &keys[2]])).unwrap());
      assert!(!v.is_sealed());
    }

    #[test]
    fn duplicate_key_does_not_unseal() {
      let v = start_vault(false);
      let keys = v.init(5, 3);
      assert!(unseal(&v.config([&keys[0], &keys[0], &keys[0]])).is_err());
      assert!(v.is_sealed());
    }

    #[test]
    fn invalid_key_errors() {
      let v = start_vault(false);
      v.init(5, 3);
      let err = unseal(&v.config(["zz", "zz", "zz"])).unwrap_err();
      assert!(err.to_string().contains("400 Bad Request"), "{err}");
      assert!(v.is_sealed());
    }

    #[test]
    fn wrong_keys_stay_sealed() {
      // valid hex, wrong shares: vault accepts them but cannot reconstruct the key
      let v = start_vault(false);
      v.init(5, 3);
      let bad = "00".repeat(33);
      assert!(unseal(&v.config([&bad, &bad, &bad])).is_err());
      assert!(v.is_sealed());
    }

    #[test]
    fn uninitialized_vault_is_noop() {
      // uninitialized vault reports 501, not 503
      let v = start_vault(false);
      assert!(!unseal(&v.config(["a", "b", "c"])).unwrap());
    }

    #[test]
    fn run_unseals_real_vault() {
      let v = start_vault(false);
      let keys = v.init(3, 3);
      let pairs = [
        ("VAULT_URL", v.url.as_str()),
        ("KEY_1", &keys[0]),
        ("KEY_2", &keys[1]),
        ("KEY_3", &keys[2]),
      ];
      let (code, out) = run_until(SIGHUP, env(&pairs), || {
        while v.is_sealed() {
          sleep(INTERVAL);
        }
        // let a few idle iterations run against the unsealed vault
        sleep(INTERVAL * 5);
      });
      assert_eq!(code, 0);
      assert_eq!(out, "Unlock successful\nGot exit signal\n");
    }

    #[test]
    fn tls_with_ca_unseals() {
      let v = start_vault(true);
      let keys = v.init(3, 3);
      let mut c = v.config([&keys[0], &keys[1], &keys[2]]);
      c.ca_cert = Some(CA.into());
      assert!(unseal(&c).unwrap());
      assert!(!v.is_sealed());
    }

    #[test]
    fn tls_without_ca_errors() {
      let v = start_vault(true);
      let keys = v.init(3, 3);
      let err = unseal(&v.config([&keys[0], &keys[1], &keys[2]])).unwrap_err();
      assert!(err.to_string().contains("UnknownIssuer"), "{err}");
      assert!(v.is_sealed());
    }

    #[test]
    fn tls_with_wrong_ca_errors() {
      let v = start_vault(true);
      let keys = v.init(3, 3);
      let mut c = v.config([&keys[0], &keys[1], &keys[2]]);
      c.ca_cert = Some(OTHER_CA.into());
      let err = unseal(&c).unwrap_err();
      assert!(err.to_string().contains("UnknownIssuer"), "{err}");
      assert!(v.is_sealed());
    }
  }
}
