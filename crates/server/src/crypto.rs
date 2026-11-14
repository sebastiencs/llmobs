//! Sealed columns: AES-256-GCM with a key id and row-bound associated data (D-I-1, D-I-2).
#![cfg_attr(not(test), allow(dead_code))] // seal and rotation are used from I1.3 on

use std::fmt;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::array::Array;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::auth::secret::{constant_time_eq, random_bytes};
use crate::meta::instance_settings;

/// The `origin` of a key that came from the environment.
const ENV_ORIGIN: &str = "LLMOBS_ENCRYPTION_KEY";

/// Label of the key id HMAC (D-I-2).
const KEY_ID_LABEL: &[u8] = b"llmobs.key-id.v1";

/// Label of the check value HMAC (D-I-2).
const CHECK_LABEL: &[u8] = b"llmobs.encryption-check.v1";

/// Where the key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeySource {
    /// `LLMOBS_ENCRYPTION_KEY` or `encryption_key`.
    Env,
    /// `<data_dir>/secret.key`, read.
    File,
    /// `<data_dir>/secret.key`, created by this start.
    Generated,
}

/// A loaded key. `Debug` prints the source and the path, never the key.
pub(crate) struct LoadedKey {
    pub(crate) key: [u8; 32],
    pub(crate) source: KeySource,
    /// `<data_dir>/secret.key`, also when the source is `Env`.
    pub(crate) path: PathBuf,
}

/// Associated data: `table:column:row_id`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Aad<'a> {
    pub(crate) table: &'static str,
    pub(crate) column: &'static str,
    pub(crate) row_id: &'a Uuid,
}

impl Aad<'_> {
    fn text(&self) -> String {
        format!("{}:{}:{}", self.table, self.column, self.row_id)
    }
}

/// Seals and opens values with one key. `Debug` prints the key id only.
pub(crate) struct SecretBox {
    cipher: Aes256Gcm,
    key_id: [u8; 4],
    check: String,
}

/// Why a key could not be used, or a value could not be opened. No variant holds key bytes,
/// plaintext or ciphertext.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The value is not 32 bytes of standard base64.
    #[error("encryption key from {origin} must be 32 bytes in standard base64")]
    BadKey {
        /// `LLMOBS_ENCRYPTION_KEY`, the key file's path, or the rotation input's name.
        origin: String,
    },
    /// A file could not be read or written.
    #[error("could not {action} {}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    /// The key does not match the check value.
    #[error("the encryption key does not match this data directory (check LLMOBS_ENCRYPTION_KEY or {})", .path.display())]
    Mismatch { path: PathBuf },
    /// `meta.db` has a check value and no key is available.
    #[error("no encryption key: set LLMOBS_ENCRYPTION_KEY or restore {} from the backup", .path.display())]
    Missing { path: PathBuf },
    /// A sealed value was written with another key.
    #[error("a sealed value was written with another key")]
    WrongKey,
    /// A sealed value is corrupt.
    #[error("a sealed value is corrupt")]
    Corrupt,
    /// The OS random source failed.
    #[error("random number generator failed")]
    Rng,
    /// `meta.db` failed.
    #[error(transparent)]
    Meta(#[from] crate::meta::MetaError),
}

/// Every sealed column, for key rotation (D-I-3). Later milestones append rows.
pub(crate) const ENCRYPTED_COLUMNS: &[(&str, &str)] = &[
    ("llm_connections", "secret_enc"),
    ("llm_connections", "extra_headers_enc"),
];

/// `<data_dir>/secret.key`.
pub(crate) fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secret.key")
}

impl SecretBox {
    pub(crate) fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new(&Array(*key)),
            key_id: key_id_of(key),
            check: hex::encode(mac(key, CHECK_LABEL)),
        }
    }

    /// Lowercase hex HMAC-SHA256(key, "llmobs.encryption-check.v1").
    pub(crate) fn check_value(&self) -> &str {
        &self.check
    }

    pub(crate) fn seal(&self, plaintext: &[u8], aad: Aad<'_>) -> Result<Vec<u8>, CryptoError> {
        let nonce_bytes = nonce()?;
        let sealed = self
            .cipher
            .encrypt(
                &Array(nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: aad.text().as_bytes(),
                },
            )
            // Fails only on absurd lengths.
            .map_err(|_error| CryptoError::Corrupt)?;
        let mut out = Vec::with_capacity(1 + 4 + sealed.len());
        out.push(0x01);
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub(crate) fn open(&self, sealed: &[u8], aad: Aad<'_>) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < 1 + 4 + 12 + 16 || sealed[0] != 0x01 {
            return Err(CryptoError::Corrupt);
        }
        if sealed[1..5] != self.key_id {
            return Err(CryptoError::WrongKey);
        }
        // The length was checked above, so this never fails; it still answers
        // `Corrupt` instead of panicking.
        let nonce_bytes: [u8; 12] = sealed
            .get(5..17)
            .and_then(|slice| <[u8; 12]>::try_from(slice).ok())
            .ok_or(CryptoError::Corrupt)?;
        self.cipher
            .decrypt(
                &Array(nonce_bytes),
                Payload {
                    msg: &sealed[17..],
                    aad: aad.text().as_bytes(),
                },
            )
            .map_err(|_error| CryptoError::Corrupt)
    }
}

impl fmt::Debug for SecretBox {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretBox")
            .field("key_id", &hex::encode(self.key_id))
            .finish()
    }
}

impl fmt::Debug for LoadedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedKey")
            .field("source", &self.source)
            .field("path", &self.path)
            .finish()
    }
}

/// HMAC-SHA256 of `label` under `key`.
fn mac(key: &[u8; 32], label: &[u8]) -> [u8; 32] {
    // `Hmac<Sha256>` accepts keys of any length, so this cannot fail.
    #[allow(clippy::expect_used)]
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length is valid");
    mac.update(label);
    mac.finalize().into_bytes().into()
}

/// HMAC-SHA256(key, "llmobs.key-id.v1")[0..4].
fn key_id_of(key: &[u8; 32]) -> [u8; 4] {
    let tag = mac(key, KEY_ID_LABEL);
    [tag[0], tag[1], tag[2], tag[3]]
}

/// A fresh 12-byte nonce (§ Sealing, "Random bytes").
fn nonce() -> Result<[u8; 12], CryptoError> {
    let bytes = random_bytes(12).map_err(|_error| CryptoError::Rng)?;
    <[u8; 12]>::try_from(bytes.as_slice()).map_err(|_bytes| CryptoError::Rng)
}

/// Decodes `text` (standard base64, padding required, `base64::engine::general_purpose::STANDARD`)
/// into 32 bytes. Any other length or a decode error is `BadKey { origin }`.
pub(crate) fn decode_key(text: &str, origin: &str) -> Result<[u8; 32], CryptoError> {
    let decoded = STANDARD
        .decode(text)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
    decoded.ok_or_else(|| CryptoError::BadKey {
        origin: origin.to_owned(),
    })
}

/// Standard base64 of `key`.
pub(crate) fn encode_key(key: &[u8; 32]) -> String {
    STANDARD.encode(key)
}

/// 32 random bytes (§ Sealing, "Random bytes").
pub(crate) fn random_key() -> Result<[u8; 32], CryptoError> {
    let bytes = random_bytes(32).map_err(|_error| CryptoError::Rng)?;
    bytes.try_into().map_err(|_bytes: Vec<u8>| CryptoError::Rng)
}

/// The key file's text with one trailing `\n` (and a `\r` before it) removed.
fn key_file_text(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
}

/// Reads and decodes the key file, warning when its mode is too wide (§ Key file, "Read").
fn read_key_file(path: &Path) -> Result<[u8; 32], CryptoError> {
    let metadata = std::fs::metadata(path).map_err(|error| CryptoError::Io {
        action: "read",
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.permissions().mode() & 0o077 != 0 {
        tracing::warn!(path = %path.display(), "encryption key file is readable by other users");
    }
    let text = std::fs::read_to_string(path).map_err(|error| CryptoError::Io {
        action: "read",
        path: path.to_path_buf(),
        error,
    })?;
    decode_key(key_file_text(&text), &path.display().to_string())
}

/// `path` with `.tmp` appended to its file name.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Removes a leftover `secret.key*.tmp`. Its absence is fine; any other error is
/// returned, so a stale tmp with a wider mode cannot survive and receive the key.
fn remove_if_present(path: &Path) -> Result<(), CryptoError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CryptoError::Io {
            action: "remove",
            path: path.to_path_buf(),
            error,
        }),
    }
}

/// Flushes a directory change to disk.
fn sync_dir(dir: &Path) -> Result<(), CryptoError> {
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| CryptoError::Io {
            action: "sync",
            path: dir.to_path_buf(),
            error,
        })
}

/// § Key file, "Write". A crash leaves either no file or a whole one.
pub(crate) fn write_key_file(path: &Path, key: &[u8; 32]) -> Result<(), CryptoError> {
    let tmp = tmp_path(path);
    // A crash can leave a leftover with a wider mode.
    remove_if_present(&tmp)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|error| CryptoError::Io {
            action: "create",
            path: tmp.clone(),
            error,
        })?;
    file.write_all(format!("{}\n", encode_key(key)).as_bytes())
        .map_err(|error| CryptoError::Io {
            action: "write",
            path: tmp.clone(),
            error,
        })?;
    file.sync_all().map_err(|error| CryptoError::Io {
        action: "sync",
        path: tmp.clone(),
        error,
    })?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| CryptoError::Io {
        action: "rename",
        path: path.to_path_buf(),
        error,
    })?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// The env value, else the file. `Ok(None)` when neither exists. Never writes.
pub(crate) fn read_key(
    env_value: Option<&str>,
    data_dir: &Path,
) -> Result<Option<LoadedKey>, CryptoError> {
    if let Some(value) = env_value {
        let key = decode_key(value, ENV_ORIGIN)?;
        return Ok(Some(LoadedKey {
            key,
            source: KeySource::Env,
            path: key_path(data_dir),
        }));
    }
    let path = key_path(data_dir);
    if !path.is_file() {
        return Ok(None);
    }
    read_key_file(&path).map(|key| {
        Some(LoadedKey {
            key,
            source: KeySource::File,
            path,
        })
    })
}

/// § Key file, "Finishing a rotation". `check` is the stored check value, if any.
pub(crate) fn finish_rotation(data_dir: &Path, check: Option<&str>) -> Result<(), CryptoError> {
    remove_if_present(&tmp_path(&key_path(data_dir)))?;
    let new_path = data_dir.join("secret.key.new");
    remove_if_present(&tmp_path(&new_path))?;
    if !new_path.is_file() {
        return Ok(());
    }
    // A file that cannot be read, or whose key does not decode or match, counts as
    // "does not match": it is deleted and startup continues.
    let matches = match std::fs::read_to_string(&new_path) {
        Ok(text) => decode_key(key_file_text(&text), &new_path.display().to_string())
            .ok()
            .is_some_and(|key| {
                check.is_some_and(|stored| {
                    constant_time_eq(stored, SecretBox::new(&key).check_value())
                })
            }),
        // It vanished between the check above and the read.
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(_error) => false,
    };
    if matches {
        std::fs::rename(&new_path, key_path(data_dir)).map_err(|error| CryptoError::Io {
            action: "rename",
            path: new_path.clone(),
            error,
        })?;
        sync_dir(data_dir)?;
        tracing::warn!("finished an interrupted key rotation");
    } else {
        remove_if_present(&new_path)?;
        tracing::warn!("removed an unused secret.key.new");
    }
    Ok(())
}

/// `read_key`; when it finds nothing, a new key file when `has_check` is false, else `Missing`.
pub(crate) fn load_key(
    env_value: Option<&str>,
    data_dir: &Path,
    has_check: bool,
) -> Result<LoadedKey, CryptoError> {
    if let Some(loaded) = read_key(env_value, data_dir)? {
        return Ok(loaded);
    }
    let path = key_path(data_dir);
    if has_check {
        return Err(CryptoError::Missing { path });
    }
    let key = random_key()?;
    write_key_file(&path, &key)?;
    Ok(LoadedKey {
        key,
        source: KeySource::Generated,
        path,
    })
}

/// The outcome of [`verify_or_record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    Recorded,
    Matched,
}

/// What [`MetaDb::write`](crate::meta::MetaDb::write)'s closure found.
enum Found {
    Recorded,
    Same,
    Different,
}

/// Stores the check value when there is none; otherwise compares it. A difference is `Mismatch`.
pub(crate) async fn verify_or_record(
    meta: &crate::meta::MetaDb,
    secrets: &SecretBox,
    path: &Path,
) -> Result<CheckOutcome, CryptoError> {
    let check = secrets.check_value().to_owned();
    let found = meta
        .write(move |conn| {
            Box::pin(async move {
                match instance_settings::get_setting_tx(conn, instance_settings::ENCRYPTION_CHECK)
                    .await?
                {
                    None => {
                        instance_settings::put_setting(
                            conn,
                            instance_settings::ENCRYPTION_CHECK,
                            &check,
                        )
                        .await?;
                        Ok(Found::Recorded)
                    }
                    Some(stored) => Ok(if constant_time_eq(&stored, &check) {
                        Found::Same
                    } else {
                        Found::Different
                    }),
                }
            })
        })
        .await?;
    match found {
        Found::Recorded => Ok(CheckOutcome::Recorded),
        Found::Same => Ok(CheckOutcome::Matched),
        Found::Different => Err(CryptoError::Mismatch {
            path: path.to_path_buf(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aad_for(row_id: &Uuid) -> Aad<'_> {
        Aad {
            table: "llm_connections",
            column: "secret_enc",
            row_id,
        }
    }

    #[test]
    fn seal_then_open_round_trips() {
        let row = Uuid::now_v7();
        let aad = aad_for(&row);
        let secrets = SecretBox::new(&[7; 32]);
        let sealed = secrets.seal(b"sk-test", aad).unwrap();
        assert_eq!(secrets.open(&sealed, aad).unwrap(), b"sk-test");
        let other = secrets.seal(b"sk-test", aad).unwrap();
        assert_ne!(sealed, other);
    }

    #[test]
    fn open_with_other_aad_fails() {
        let row = Uuid::now_v7();
        let other_row = Uuid::now_v7();
        let secrets = SecretBox::new(&[7; 32]);
        let sealed = secrets.seal(b"sk-test", aad_for(&row)).unwrap();
        assert!(matches!(
            secrets.open(&sealed, aad_for(&other_row)),
            Err(CryptoError::Corrupt)
        ));
        let other_column = Aad {
            table: "llm_connections",
            column: "extra_headers_enc",
            row_id: &row,
        };
        assert!(matches!(
            secrets.open(&sealed, other_column),
            Err(CryptoError::Corrupt)
        ));
    }

    #[test]
    fn open_with_other_key_is_wrong_key() {
        let row = Uuid::now_v7();
        let aad = aad_for(&row);
        let sealed = SecretBox::new(&[1; 32]).seal(b"sk-test", aad).unwrap();
        assert!(matches!(
            SecretBox::new(&[2; 32]).open(&sealed, aad),
            Err(CryptoError::WrongKey)
        ));
    }

    #[test]
    fn open_rejects_short_and_bad_version() {
        let row = Uuid::now_v7();
        let aad = aad_for(&row);
        let secrets = SecretBox::new(&[7; 32]);
        assert!(matches!(secrets.open(&[], aad), Err(CryptoError::Corrupt)));

        let mut bad_version = secrets.seal(b"sk-test", aad).unwrap();
        bad_version[0] = 0x02;
        assert!(matches!(
            secrets.open(&bad_version, aad),
            Err(CryptoError::Corrupt)
        ));

        let mut flipped = secrets.seal(b"sk-test", aad).unwrap();
        let last = flipped.len() - 1;
        flipped[last] ^= 0xff;
        assert!(matches!(
            secrets.open(&flipped, aad),
            Err(CryptoError::Corrupt)
        ));
    }

    #[test]
    fn sealed_layout() {
        let row = Uuid::now_v7();
        let aad = aad_for(&row);
        let secrets = SecretBox::new(&[7; 32]);
        let sealed = secrets.seal(b"sk-test", aad).unwrap();
        assert_eq!(sealed.len(), 1 + 4 + 12 + b"sk-test".len() + 16);
        assert_eq!(sealed[0], 1);
        assert_eq!(&sealed[1..5], &secrets.key_id);
    }

    #[test]
    fn check_value_is_stable() {
        let first = SecretBox::new(&[0; 32]);
        let value = first.check_value();
        assert_eq!(value.len(), 64);
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "{value}"
        );
        assert_eq!(value, SecretBox::new(&[0; 32]).check_value());
        assert_ne!(value, SecretBox::new(&[1; 32]).check_value());
    }

    #[test]
    fn decode_key_rules() {
        let key = [9; 32];
        assert_eq!(decode_key(&encode_key(&key), "origin").unwrap(), key);

        let short = STANDARD.encode([9u8; 31]);
        assert!(matches!(
            decode_key(&short, "origin"),
            Err(CryptoError::BadKey { .. })
        ));

        let error = decode_key("not base64!", "origin").unwrap_err();
        assert!(matches!(error, CryptoError::BadKey { .. }));
        assert!(!error.to_string().contains("not base64!"));
    }

    #[test]
    fn load_key_generates_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_key(None, dir.path(), false).unwrap();
        assert_eq!(first.source, KeySource::Generated);

        let path = dir.path().join("secret.key");
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.len(), 45);
        assert!(content.ends_with('\n'));
        assert!(!dir.path().join("secret.key.tmp").exists());

        let second = load_key(None, dir.path(), true).unwrap();
        assert_eq!(second.source, KeySource::File);
        assert_eq!(second.key, first.key);
    }

    #[test]
    fn load_key_prefers_env() {
        let dir = tempfile::tempdir().unwrap();
        let key = [5; 32];
        let loaded = load_key(Some(&encode_key(&key)), dir.path(), false).unwrap();
        assert_eq!(loaded.source, KeySource::Env);
        assert_eq!(loaded.key, key);
        assert!(!dir.path().join("secret.key").exists());
    }

    #[test]
    fn load_key_refuses_to_generate_after_a_check() {
        let dir = tempfile::tempdir().unwrap();
        let error = load_key(None, dir.path(), true).unwrap_err();
        assert!(matches!(error, CryptoError::Missing { .. }));
        assert!(!dir.path().join("secret.key").exists());
    }

    #[test]
    fn load_key_rejects_a_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.key"), "not base64\n").unwrap();
        let error = load_key(None, dir.path(), false).unwrap_err();
        assert!(matches!(error, CryptoError::BadKey { .. }));
        assert!(
            error
                .to_string()
                .contains(&dir.path().display().to_string()),
            "{error}"
        );
    }

    #[test]
    fn finish_rotation_renames_a_matching_new_key() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(&dir.path().join("secret.key"), &[1; 32]).unwrap();
        let new = [2; 32];
        write_key_file(&dir.path().join("secret.key.new"), &new).unwrap();
        let check = SecretBox::new(&new).check_value().to_owned();
        finish_rotation(dir.path(), Some(&check)).unwrap();
        assert!(!dir.path().join("secret.key.new").exists());
        let text = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        assert_eq!(decode_key(key_file_text(&text), "test").unwrap(), new);
    }

    #[test]
    fn finish_rotation_deletes_a_stale_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let old = [1; 32];
        write_key_file(&dir.path().join("secret.key"), &old).unwrap();
        write_key_file(&dir.path().join("secret.key.new"), &[2; 32]).unwrap();
        let check = SecretBox::new(&old).check_value().to_owned();
        finish_rotation(dir.path(), Some(&check)).unwrap();
        assert!(!dir.path().join("secret.key.new").exists());
        let text = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        assert_eq!(decode_key(key_file_text(&text), "test").unwrap(), old);
    }

    #[test]
    fn finish_rotation_removes_leftover_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(&dir.path().join("secret.key"), &[1; 32]).unwrap();
        std::fs::write(dir.path().join("secret.key.tmp"), "leftover\n").unwrap();
        std::fs::write(dir.path().join("secret.key.new.tmp"), "leftover\n").unwrap();
        finish_rotation(dir.path(), None).unwrap();
        assert!(!dir.path().join("secret.key.tmp").exists());
        assert!(!dir.path().join("secret.key.new.tmp").exists());
    }

    #[test]
    fn finish_rotation_deletes_an_undecodable_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let old = [1; 32];
        write_key_file(&dir.path().join("secret.key"), &old).unwrap();
        std::fs::write(dir.path().join("secret.key.new"), "not base64\n").unwrap();
        let check = SecretBox::new(&old).check_value().to_owned();
        finish_rotation(dir.path(), Some(&check)).unwrap();
        assert!(!dir.path().join("secret.key.new").exists());
        let text = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        assert_eq!(decode_key(key_file_text(&text), "test").unwrap(), old);
    }

    #[test]
    fn finish_rotation_without_a_check_deletes_a_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let old = [1; 32];
        write_key_file(&dir.path().join("secret.key"), &old).unwrap();
        write_key_file(&dir.path().join("secret.key.new"), &[2; 32]).unwrap();
        finish_rotation(dir.path(), None).unwrap();
        assert!(!dir.path().join("secret.key.new").exists());
        let text = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        assert_eq!(decode_key(key_file_text(&text), "test").unwrap(), old);
    }

    #[test]
    fn load_key_reads_a_file_with_a_wide_mode() {
        let dir = tempfile::tempdir().unwrap();
        let key = [3; 32];
        write_key_file(&dir.path().join("secret.key"), &key).unwrap();
        std::fs::set_permissions(
            dir.path().join("secret.key"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let loaded = load_key(None, dir.path(), true).unwrap();
        assert_eq!(loaded.source, KeySource::File);
        assert_eq!(loaded.key, key);
    }

    #[test]
    fn write_key_file_replaces_a_leftover_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("secret.key.tmp");
        std::fs::write(&tmp, "leftover\n").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.path().join("secret.key");
        write_key_file(&path, &[4; 32]).unwrap();
        assert!(!tmp.exists());
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn encrypted_columns_cover_the_schema() {
        let (_dir, db) = crate::meta::test_db().await;
        let mut rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT m.name, p.name FROM sqlite_master m JOIN pragma_table_info(m.name) p \
             WHERE m.type = 'table' AND p.name LIKE '%\\_enc' ESCAPE '\\'",
        )
        .fetch_all(db.reader())
        .await
        .unwrap();
        rows.sort();
        let mut expected: Vec<(String, String)> = ENCRYPTED_COLUMNS
            .iter()
            .map(|(table, column)| ((*table).to_owned(), (*column).to_owned()))
            .collect();
        expected.sort();
        assert_eq!(rows, expected);
    }

    #[tokio::test]
    async fn verify_or_record_accepts_same_and_refuses_other() {
        let (dir, db) = crate::meta::test_db().await;
        let path = key_path(dir.path());
        let first = SecretBox::new(&[1; 32]);
        let second = SecretBox::new(&[2; 32]);
        assert_eq!(
            verify_or_record(&db, &first, &path).await.unwrap(),
            CheckOutcome::Recorded
        );
        assert_eq!(
            verify_or_record(&db, &first, &path).await.unwrap(),
            CheckOutcome::Matched
        );
        assert!(matches!(
            verify_or_record(&db, &second, &path).await,
            Err(CryptoError::Mismatch { .. })
        ));
    }
}
