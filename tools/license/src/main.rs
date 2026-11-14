// SPDX-License-Identifier: Elastic-2.0
//! Signs llmobs licence keys and entitlement tokens with a seed file (D-P-5, D-P-6).
//!
//! Development fixtures and trial licences only; the tool is never shipped. Its output
//! format is the one `crates/server/src/ee/token.rs` verifies: `llmobs1.<kid>.<payload>.<signature>`.

// A CLI tool prints its result to stdout and its errors to stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey};

/// Format tag. The server's `TOKEN_TAG` holds the same value.
const TAG: &str = "llmobs1";

/// The development key id, trusted by debug builds only.
const DEV_KID: &str = "dev-1";

/// The fixture directory, relative to this crate.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/license");

/// Why a command failed. Messages name files, never seed or payload contents.
#[derive(Debug, thiserror::Error)]
enum ToolError {
    #[error("cannot read or write {path}")]
    Io { path: String },
    #[error("{path} is not a base64 ed25519 seed of 32 bytes")]
    BadSeed { path: String },
    #[error("{path} is not valid JSON")]
    BadPayload { path: String },
    #[error("the payload is not a JSON object")]
    NotObject,
    #[error("the payload cannot be encoded")]
    Encode,
    #[error("the kid must match [a-z0-9-]{{1,32}}")]
    BadKid,
    #[error("token files differ from their payloads: {}", files.join(", "))]
    Differs { files: Vec<String> },
}

#[derive(Debug, Parser)]
#[command(
    name = "llmobs-license",
    about = "Sign llmobs licence keys (development and trials)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prints a token whose payload is the JSON object in PAYLOAD, signed with the seed in KEY.
    Sign {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        kid: String,
        #[arg(long)]
        payload: PathBuf,
    },
    /// Prints the public key of the seed in KEY (standard base64).
    PublicKey {
        #[arg(long)]
        key: PathBuf,
    },
    /// Signs every fixtures/license/payloads/<name>.json with dev-1 into tokens/<name>.token.
    Fixtures {
        /// Writes nothing; fails when a token file differs, is missing, or has no payload.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("llmobs-license: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), ToolError> {
    match command {
        Command::Sign { key, kid, payload } => {
            let seed = read_seed(&key)?;
            let payload = read_payload(&payload)?;
            println!("{}", sign(&seed, &kid, &payload)?);
        }
        Command::PublicKey { key } => println!("{}", public_key(&read_seed(&key)?)),
        Command::Fixtures { check } => fixtures(check)?,
    }
    Ok(())
}

/// Signs `payload` as `llmobs1.<kid>.<base64url(json)>.<base64url(signature)>`.
fn sign(seed: &[u8; 32], kid: &str, payload: &serde_json::Value) -> Result<String, ToolError> {
    if !valid_kid(kid) {
        return Err(ToolError::BadKid);
    }
    if !payload.is_object() {
        return Err(ToolError::NotObject);
    }
    let json = serde_json::to_vec(payload).map_err(|_| ToolError::Encode)?;
    let signed = format!("{TAG}.{kid}.{}", URL_SAFE_NO_PAD.encode(json));
    let signature = SigningKey::from_bytes(seed).sign(signed.as_bytes());
    Ok(format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

/// `[a-z0-9-]{1,32}`, the same rule the server applies to kids.
fn valid_kid(kid: &str) -> bool {
    (1..=32).contains(&kid.len())
        && kid
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn public_key(seed: &[u8; 32]) -> String {
    STANDARD.encode(SigningKey::from_bytes(seed).verifying_key().to_bytes())
}

fn read_seed(path: &Path) -> Result<[u8; 32], ToolError> {
    let shown = path.display().to_string();
    let text = fs::read_to_string(path).map_err(|_| ToolError::Io {
        path: shown.clone(),
    })?;
    let bytes = STANDARD
        .decode(text.trim())
        .map_err(|_| ToolError::BadSeed {
            path: shown.clone(),
        })?;
    <[u8; 32]>::try_from(bytes).map_err(|_| ToolError::BadSeed { path: shown })
}

fn read_payload(path: &Path) -> Result<serde_json::Value, ToolError> {
    let shown = path.display().to_string();
    let text = fs::read_to_string(path).map_err(|_| ToolError::Io {
        path: shown.clone(),
    })?;
    serde_json::from_str(&text).map_err(|_| ToolError::BadPayload { path: shown })
}

/// The file names in `dir` with extension `extension`, without the extension, sorted.
fn stems(dir: &Path, extension: &str) -> Result<Vec<String>, ToolError> {
    let io = || ToolError::Io {
        path: dir.display().to_string(),
    };
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|_| io())? {
        let path = entry.map_err(|_| io())?.path();
        if path.extension().and_then(|e| e.to_str()) == Some(extension)
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Signs every payload with the dev seed, writing the tokens or, with `check`, failing when
/// they differ from what the payloads sign to.
fn fixtures(check: bool) -> Result<(), ToolError> {
    let root = Path::new(FIXTURES);
    let seed = read_seed(&root.join("dev-1.key"))?;
    let payloads_dir = root.join("payloads");
    let tokens_dir = root.join("tokens");
    let names = stems(&payloads_dir, "json")?;

    let mut differs = Vec::new();
    for name in &names {
        let payload = read_payload(&payloads_dir.join(format!("{name}.json")))?;
        let token = format!("{}\n", sign(&seed, DEV_KID, &payload)?);
        let path = tokens_dir.join(format!("{name}.token"));
        if check {
            if fs::read_to_string(&path).ok().as_deref() != Some(token.as_str()) {
                differs.push(format!("tokens/{name}.token"));
            }
        } else {
            fs::write(&path, token).map_err(|_| ToolError::Io {
                path: path.display().to_string(),
            })?;
        }
    }
    if check {
        for name in stems(&tokens_dir, "token")? {
            if !names.contains(&name) {
                differs.push(format!("tokens/{name}.token"));
            }
        }
        if !differs.is_empty() {
            return Err(ToolError::Differs { files: differs });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fixtures_are_up_to_date() {
        fixtures(true).unwrap();
    }

    #[test]
    fn sign_refuses_bad_input() {
        let seed = [7_u8; 32];
        assert!(matches!(
            sign(&seed, "Prod-1", &json!({})),
            Err(ToolError::BadKid)
        ));
        assert!(matches!(
            sign(&seed, "prod-1", &json!([1])),
            Err(ToolError::NotObject)
        ));
    }

    #[test]
    fn public_key_of_dev_seed() {
        let seed = read_seed(&Path::new(FIXTURES).join("dev-1.key")).unwrap();
        assert_eq!(
            public_key(&seed),
            "kMTjOGaR2ry6zgwZ2p2nk0kMsOoyisgt3qfRm4JH36Y="
        );
    }
}
