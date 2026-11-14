//! The credentials file of `llmobs login` (spec 08): one saved personal access token per server URL, mode 0600.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct CredentialsFile {
    #[serde(default)]
    pub(crate) servers: BTreeMap<String, SavedToken>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SavedToken {
    pub(crate) token: String,
}

pub(crate) fn credentials_path() -> anyhow::Result<PathBuf> {
    let config = dirs::config_dir().context("finding the config directory")?;
    Ok(config.join("llmobs").join("credentials.toml"))
}

pub(crate) fn load(path: &Path) -> anyhow::Result<CredentialsFile> {
    if !path.exists() {
        return Ok(CredentialsFile::default());
    }
    let text = fs::read_to_string(path).context("reading credentials")?;
    toml::from_str(&text).map_err(|_| anyhow::anyhow!("credentials file is invalid"))
}

pub(crate) fn save(path: &Path, file: &CredentialsFile) -> anyhow::Result<()> {
    let directory = path.parent().context("credentials path has no directory")?;
    fs::create_dir_all(directory).context("creating the credentials directory")?;
    set_dir_mode(directory)?;
    let text = toml::to_string(file).map_err(|_| anyhow::anyhow!("encoding credentials"))?;
    let temporary = temporary_path(path);
    write_private(&temporary, text.as_bytes())?;
    fs::rename(&temporary, path).context("replacing credentials")?;
    Ok(())
}

/// Drops a trailing slash so one server is one map key.
pub(crate) fn normalize_server_url(url: &str) -> String {
    let mut url = url.trim().to_owned();
    while url.ends_with('/') {
        url.pop();
    }
    url
}

/// Whether a server URL is usable, checked by every path a URL can arrive on (flag,
/// environment variable, credentials file, `login` argument): it must parse as an `http` or
/// `https` URL and carry no `user[:password]@`. A URL with credentials would be stored,
/// printed and requested with the password inside, so it is refused instead; the error never
/// includes the URL.
pub(crate) fn check_server_url(url: &str) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| anyhow::anyhow!("--url must start with http:// or https://"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("--url must start with http:// or https://");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("the server URL must not contain credentials");
    }
    Ok(())
}

pub(crate) fn chosen_server(
    server_url: Option<&str>,
    file: &CredentialsFile,
) -> anyhow::Result<String> {
    if let Some(url) = server_url {
        let server = normalize_server_url(url);
        check_server_url(&server)?;
        return Ok(server);
    }
    match file.servers.len() {
        0 => anyhow::bail!("no saved server"),
        1 => {
            let Some(server) = file.servers.keys().next() else {
                anyhow::bail!("no saved server");
            };
            check_server_url(server)?;
            Ok(server.clone())
        }
        _ => anyhow::bail!("more than one saved server; pass --url"),
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials.toml");
    temporary.set_file_name(format!("{name}.tmp"));
    temporary
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).context("creating credentials")?;
    file.write_all(bytes).context("writing credentials")?;
    file.sync_all().context("writing credentials")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .context("setting the credentials file mode")?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode(directory: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .context("setting the credentials directory mode")
}

#[cfg(not(unix))]
fn set_dir_mode(_directory: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_server_url, normalize_server_url};

    #[test]
    fn server_url_loses_trailing_slashes() {
        assert_eq!(
            normalize_server_url(" http://127.0.0.1:8080/ "),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn check_server_url_refuses_credentials_and_bad_schemes() {
        let cases = [
            ("http://[::1]:8080", false),
            ("HTTPS://u@h", true),
            ("http://h?x=a@b", false),
            ("http://h#@", false),
            ("http://h/@path", false),
            ("http://u:p@ss@h", true),
            // The tab is stripped by the URL parser, leaving http://user:pw@h.
            ("http:/\t/user:pw@h", true),
            ("user:pw@h", true),
        ];
        for (url, refused) in cases {
            let checked = check_server_url(url);
            assert_eq!(checked.is_err(), refused, "{url}");
            if let Err(error) = checked {
                let text = error.to_string();
                assert!(
                    !text.contains("user") && !text.contains("pw"),
                    "{url}: {text}"
                );
            }
        }
    }
}
