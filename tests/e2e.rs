use std::{
  io::{BufRead, BufReader},
  process::{Command, Stdio},
};

use serde_json::{Value, json};
use testcontainers::{
  GenericImage, ImageExt,
  core::{IntoContainerPort, WaitFor},
  runners::SyncRunner,
};

/// Runs the real binary against a sealed vault, waits for unseal, then stops it with SIGTERM.
#[test]
fn binary_unseals_vault_and_exits_on_sigterm() {
  let container = GenericImage::new("hashicorp/vault", "2.2")
    .with_exposed_port(8200.tcp())
    .with_wait_for(WaitFor::message_on_stdout("Vault server started!"))
    .with_env_var("SKIP_SETCAP", "true")
    .with_env_var(
      "VAULT_LOCAL_CONFIG",
      r#"{"storage":{"inmem":{}},"listener":{"tcp":{"address":"0.0.0.0:8200","tls_disable":true}},"disable_mlock":true}"#,
    )
    .with_cmd(["server"])
    .start()
    .expect("failed to start vault container");
  let url = format!(
    "http://{}:{}",
    container.get_host().unwrap(),
    container.get_host_port_ipv4(8200).unwrap()
  );

  let init: Value = ureq::put(format!("{url}/v1/sys/init"))
    .send_json(json!({ "secret_shares": 3, "secret_threshold": 3 }))
    .unwrap()
    .body_mut()
    .read_json()
    .unwrap();
  let keys = &init["keys"];

  let mut child = Command::new(env!("CARGO_BIN_EXE_vault-auto-unseal"))
    .env_remove("CA_CERT")
    .env("VAULT_URL", &url)
    .env("KEY_1", keys[0].as_str().unwrap())
    .env("KEY_2", keys[1].as_str().unwrap())
    .env("KEY_3", keys[2].as_str().unwrap())
    .stdout(Stdio::piped())
    .spawn()
    .unwrap();
  let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

  assert_eq!(lines.next().unwrap().unwrap(), "Unlock successful");
  let status: Value = ureq::get(format!("{url}/v1/sys/seal-status"))
    .call()
    .unwrap()
    .body_mut()
    .read_json()
    .unwrap();
  assert_eq!(status["sealed"], false);

  let killed = Command::new("kill")
    .args(["-TERM", &child.id().to_string()])
    .status()
    .unwrap();
  assert!(killed.success());
  assert_eq!(lines.next().unwrap().unwrap(), "Got exit signal");
  assert!(child.wait().unwrap().success());
}
