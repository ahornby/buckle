use anyhow::{anyhow, bail, Context, Error};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    time::SystemTime,
};
use url::Url;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod config;

use config::{ArchiveConfig, BuckleConfig, BuckleSource, GithubRelease, PackageType};

const BUCKLE_BINARY: &str = "BUCKLE_BINARY";
const BUCKLE_CONFIG: &str = "BUCKLE_CONFIG";
const BUCKLE_CONFIG_FILE: &str = "BUCKLE_CONFIG_FILE";
const BUCKLE_SCRIPT: &str = "BUCKLE_SCRIPT";
const BUCKLE_HOME: &str = "BUCKLE_HOME";
const BUCKLE_REPO_CONFIG: &str = ".buckleconfig.toml";

fn get_buckle_dir() -> Result<PathBuf, Error> {
    let mut dir = match env::var(BUCKLE_HOME) {
        Ok(home) => Ok(PathBuf::from(home)),
        Err(_) => match env::consts::OS {
            "linux" => {
                if let Ok(base_dir) = env::var("XDG_CACHE_HOME") {
                    Ok(PathBuf::from(base_dir))
                } else if let Ok(base_dir) = env::var("HOME") {
                    let mut path = PathBuf::from(base_dir);
                    path.push(".cache");
                    Ok(path)
                } else {
                    Err(anyhow!("neither $XDG_CACHE_HOME nor $HOME are defined. Either define them or specify a $BUCKLE_HOME"))
                }
            }
            "macos" => {
                let mut base_dir = env::var("HOME")
                    .map(PathBuf::from)
                    .map_err(|_| anyhow!("$HOME is not defined"))?;
                base_dir.push("Library");
                base_dir.push("Caches");
                Ok(base_dir)
            }
            "windows" => Ok(env::var("LocalAppData")
                .map(PathBuf::from)
                .map_err(|_| anyhow!("%LocalAppData% is not defined"))?),
            os => Err(anyhow!(
                "'{os}' is currently an unsupported OS. Feel free to contribute a patch."
            )),
        },
    }?;
    dir.push("buckle");
    Ok(dir)
}

/// Use the most recent .buckconfig except if a .buckroot is found.
fn find_project_root() -> Result<Option<PathBuf>, Error> {
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Asset {
    pub name: String,
    pub browser_download_url: Url,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Release {
    pub url: Url,
    pub html_url: Url,
    pub assets_url: Url,
    pub upload_url: String,
    pub tarball_url: Option<Url>,
    pub zipball_url: Option<Url>,
    pub id: usize,
    pub node_id: String,
    pub tag_name: String,
    pub target_commitish: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub created_at: Option<String>,
    pub published_at: Option<String>,
    pub author: serde_json::Value,
    pub assets: Vec<Asset>,
}

fn get_releases(gh_release: &GithubRelease, path: &Path) -> Result<Vec<Release>, Error> {
    let mut releases_json_path = path.to_path_buf();
    releases_json_path.push("releases.json");

    if releases_json_path.exists() {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(&releases_json_path)?;
        let last_modification_time = meta.mtime();
        let curr_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs() as i64;
        if (curr_time - last_modification_time).abs() < 60 * 60 {
            let buf = fs::read_to_string(releases_json_path)?;
            return Ok(serde_json::from_str(&buf)?);
        }
    }

    let client = reqwest::blocking::Client::builder()
        .user_agent("buckle")
        .build()?;

    let releases = client
        .get(format!(
            "http://api.github.com/repos/{}/{}/releases",
            gh_release.owner, gh_release.repo
        ))
        .send()?;
    let text = releases.text_with_charset("utf-8")?;
    let mut file = File::create(releases_json_path)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(serde_json::from_str(&text)?)
}

// Approximate the target triple for the current platform
// as per https://rust-lang.github.io/rfcs/0131-target-specification.html
fn get_target() -> Result<&'static str, Error> {
    Ok(match env::consts::ARCH {
        "x86_64" => match env::consts::OS {
            "linux" => "x86_64-unknown-linux-gnu",
            "darwin" => "x86_64-apple-darwin",
            "windows" => "x86_64-pc-windows-msvc",
            _ => return Err(anyhow!("Unsupported Arch/OS")),
        },
        "aarch64" => match env::consts::OS {
            "linux" => "aarch64-unknown-linux-gnu",
            "darwin" => "aarch64-apple-darwin",
            _ => return Err(anyhow!("Unsupported Arch/OS")),
        },
        _ => return Err(anyhow!("Unsupported Architecture")),
    })
}

fn download_from_github(
    artifact_pattern: &str,
    gh_release: &GithubRelease,
    output_dir: &Path,
) -> Result<(PathBuf, reqwest::blocking::Response), Error> {
    let releases = get_releases(gh_release, output_dir)?;

    let mut path = output_dir.to_path_buf();
    let mut artefact = None;

    // simple templating, we'll need this even if we add regex support
    let mut artifact_name = artifact_pattern.to_string();
    artifact_name = artifact_name.replace("%version%", &gh_release.version);
    artifact_name = artifact_name.replace("%arch%", env::consts::ARCH);
    artifact_name = artifact_name.replace("%os%", env::consts::OS);
    artifact_name = artifact_name.replace("%target%", get_target()?);

    for release in releases {
        if release.name.as_ref() == Some(&gh_release.version) {
            if release.tag_name == "latest" {
                path.push(release.target_commitish);
            } else {
                // TODO use sha256 checksum if present
                path.push(release.name.ok_or_else(|| anyhow!("Release has no name"))?);
            }
            for asset in release.assets {
                let name = asset.name;
                let url = asset.browser_download_url;
                if name == artifact_name {
                    artefact = Some((name, url));
                    break;
                }
            }
        }
    }

    let (_name, url) = if let Some((name, url)) = artefact {
        (name, url)
    } else {
        bail!(
            "Could not find {}. Its possible {} has changed their releasing method for {}. Please update buckle.",
            artifact_name,
            gh_release.owner,
            gh_release.repo
        )
    };

    if !path.exists() {
        fs::create_dir_all(&path).with_context(|| anyhow!("error creating {:?}", path))?;
    }

    Ok((path, reqwest::blocking::get(url)?))
}

fn extract<R>(
    unpacked_name: &str,
    package_type: &PackageType,
    mut archive_stream: R,
    path: PathBuf,
) -> Result<PathBuf, Error>
where
    R: Read,
{
    let mut final_name = path.clone();
    final_name.push(unpacked_name);
    if final_name.exists() {
        // unpacked already present, so do nothing
        return Ok(final_name);
    }

    let mut tmp_name = path;
    // TODO(ahornby) use tmpfile crate
    tmp_name.push(format!("{}{}", unpacked_name, ".tmp"));

    let mut tmp_file =
        File::create(&tmp_name).with_context(|| anyhow!("problem opening {:?}", tmp_name))?;
    match package_type {
        PackageType::SingleFile => {
            io::copy(&mut archive_stream, &mut tmp_file)
                .with_context(|| anyhow!("problem copying to {:?}", tmp_name))?;
            tmp_file.flush()?;
        }
        PackageType::ZstdSingleFile => zstd::stream::copy_decode(archive_stream, tmp_file)?,
    }

    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(&tmp_name, permissions)?;

    // only move to final name once fully written and stable
    fs::rename(&tmp_name, &final_name)
        .with_context(|| anyhow!("problem copying to {:?}", final_name))?;
    Ok(final_name)
}

fn download(
    binary_name: &str,
    archive_config: &ArchiveConfig,
    output_dir: &Path,
) -> Result<PathBuf, Error> {
    let (path, stream) = match &archive_config.source {
        BuckleSource::Github(ref gh_release) => {
            download_from_github(&archive_config.artifact_pattern, gh_release, output_dir)?
        }
    };
    extract(binary_name, &archive_config.package_type, stream, path)
}

fn get_binary_path(config: BuckleConfig, binary_name: Option<&String>) -> Result<PathBuf, Error> {
    let binary_name = if let Some(binary_name) = binary_name {
        binary_name
    } else if config.binaries.len() == 1 {
        // only one so default to it
        config.binaries.keys().next().unwrap()
    } else {
        bail!("No binary name provided");
    };

    let bin_config = config
        .binaries
        .get(binary_name)
        .ok_or_else(|| anyhow!("No binary named {} in buckle config", binary_name))?;
    let archive_name = &bin_config.provided_by;
    let archive_config = config
        .archives
        .get(archive_name)
        .ok_or_else(|| anyhow!("No archive named {} in buckle config", archive_name))?;

    let archive_dir = get_buckle_dir()?.join(archive_name);

    if !archive_dir.exists() {
        fs::create_dir_all(&archive_dir)
            .with_context(|| anyhow!("error creating {:?}", archive_dir))?;
    }

    download(binary_name, archive_config, &archive_dir).map_err(Error::into)
}

fn load_config(args: &mut env::ArgsOs) -> Result<BuckleConfig, Error> {
    if let Ok(config) = env::var(BUCKLE_CONFIG) {
        // Short circuit if the user has given us a config in the environment
        return Ok(toml::from_str(&config)?);
    }

    let config_file = if env::var(BUCKLE_SCRIPT).is_ok() {
        Some(PathBuf::from(args.next().unwrap().to_str().unwrap()))
    } else {
        match env::var(BUCKLE_CONFIG_FILE) {
            Ok(file) => Some(PathBuf::from(file)),
            // Then try repo level config
            Err(_) => {
                if let Some(mut root) = find_project_root()? {
                    root.push(BUCKLE_REPO_CONFIG);
                    if root.exists() {
                        Some(root)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    };

    if let Some(config_file) = config_file {
        let config = &fs::read_to_string(&config_file).with_context(|| {
            format!(
                "Could not read config file {:?}",
                config_file.to_string_lossy()
            )
        })?;
        let config = toml::from_str(config)?;
        Ok(config)
    } else {
        //dbg!("No config file found, using builtin buck2 defaults");
        Ok(BuckleConfig::buck2_latest())
    }
}

// What binary are we trying to run from the buckle config?
fn get_binary_name(invoked_as: Option<&OsString>) -> Option<String> {
    if let Ok(binary_name) = env::var(BUCKLE_BINARY) {
        Some(binary_name)
    } else if let Some(invoked_as) = invoked_as {
        let base_name = Path::new(&invoked_as).file_name();
        if base_name == Some(OsStr::new("buckle")) {
            None
        } else {
            base_name.and_then(|v| v.to_str()).map(|v| v.to_string())
        }
    } else {
        None
    }
}

fn main() -> Result<(), Error> {
    // Collect information intended for invoked binary.

    // Collect information indented for buck2 binary.
    let mut args = env::args_os();
    // Figure out what binary we are trying to run.
    let invoked_as = args.next();
    let binary_name = get_binary_name(invoked_as.as_ref());
    let binary_name = binary_name;
    let buckle_config = load_config(&mut args)?;
    let binary_path = get_binary_path(buckle_config, binary_name.as_ref())?;

    if env::var(BUCKLE_SCRIPT).is_err() {
        eprintln!("buckle is running {:?}", binary_path)
    }

    // Remove so any recursive buckle calls need their own #! to be in script mode
    env::remove_var(BUCKLE_SCRIPT);
    let envs = env::vars_os();

    // Pass all file descriptors through as well.
    let status = Command::new(&binary_path)
        .args(args)
        .envs(envs)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|_| panic!("Failed to execute {:?}", &binary_path))
        .status;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
