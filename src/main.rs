use std::{
  thread::{sleep, spawn},
  time::Duration,
};

use anyhow::Result;
use serde::Serialize;
use signal_hook::{consts::TERM_SIGNALS, iterator::Signals};
use ureq::{
  Agent,
  http::StatusCode,
  tls::{Certificate, RootCerts, TlsConfig},
};

fn main() {
  let _ = if let Ok(mut s) = Signals::new(TERM_SIGNALS) {
    spawn(move || {
      let _ = s.forever().next();
      println!("Got exit signal");
      std::process::exit(0);
    })
  } else {
    println!("Failed to register signal");
    std::process::exit(1)
  };

  loop {
    if let Err(err) = unseal() {
      println!("Unseal error: {err}");
    }
    sleep(Duration::from_secs(15));
  }
}

fn unseal() -> Result<()> {
  let cert = std::env::var("CA_CERT")?;
  let cert = Certificate::from_pem(cert.as_bytes())?;
  let tls = TlsConfig::builder()
    .root_certs(RootCerts::new_with_certs(&[cert]))
    .build();
  let config = Agent::config_builder()
    .tls_config(tls)
    .http_status_as_error(false)
    .build();
  let agent = Agent::new_with_config(config);

  let vault_url = std::env::var("VAULT_URL")?;
  let res = agent.get(format!("{vault_url}/v1/sys/health")).call()?;

  if res.status() != StatusCode::SERVICE_UNAVAILABLE {
    return Ok(());
  }

  let key_1 = std::env::var("KEY_1")?;
  let key_2 = std::env::var("KEY_2")?;
  let key_3 = std::env::var("KEY_3")?;
  let keys = [key_1, key_2, key_3];

  for key in keys {
    agent
      .post(format!("{vault_url}/v1/sys/unseal"))
      .send_json(&UnsealReq { key })?;
  }

  println!("Unlock successful");

  Ok(())
}

#[derive(Serialize)]
struct UnsealReq {
  key: String,
}
