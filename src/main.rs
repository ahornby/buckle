use anyhow::{anyhow, Context, Error};
use ini::Ini;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::NamedTempFile;
use url::Url;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::time::SystemTime;

const BASE_URL: &str = "https://github.com/facebook/buck2/releases/download";
const BUCK_RELEASE_URL: &str = "https://github.com/facebook/buck2/tags";

fn get_buckle_dir() -> Result<PathBuf, Error> {
    let mut dir = match env::var("BUCKLE_CACHE") {
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
                    Err(anyhow!("neither $XDG_CACHE_HOME nor $HOME are defined. Either define them or specify a $BUCKLE_CACHE"))
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

/// Find the furthest .buckconfig except if a .buckroot is found.
fn get_buck2_project_root() -> Option<&'static Path> {
    static INSTANCE: OnceCell<Option<PathBuf>> = OnceCell::new();
    let path = INSTANCE.get_or_init(|| {
        let path = env::current_dir().unwrap();
        let mut current_root = None;
        for ancestor in path.ancestors() {
            let mut br = ancestor.to_path_buf();
            br.push(".buckroot");
            if br.exists() {
                // A buckroot means you should not check any higher in the file tree.
                return Some(ancestor.to_path_buf());
            }

            let mut bc = ancestor.to_path_buf();
            bc.push(".buckconfig");
            if bc.exists() {
                // This is the highest buckconfig we know about
                current_root = Some(ancestor.to_path_buf());
            }
        }
        current_root
    });
    path.as_deref()
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

fn get_releases(path: &Path) -> Result<Vec<Release>, Error> {
    let mut releases_json_path = path.to_path_buf();
    releases_json_path.push("releases.json");

    // TODO support last last_modification_time for windows users
    #[cfg(unix)]
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
        .get("http://api.github.com/repos/facebook/buck2/releases")
        .send()?;
    let text = releases.text_with_charset("utf-8")?;
    let mut file = File::create(releases_json_path)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(serde_json::from_str(&text)?)
}

fn get_arch() -> Result<&'static str, Error> {
    Ok(match env::consts::ARCH {
        "x86_64" => match env::consts::OS {
            "linux" => "x86_64-unknown-linux-gnu",
            "darwin" | "macos" => "x86_64-apple-darwin",
            "windows" => "x86_64-pc-windows-msvc",
            unknown => return Err(anyhow!("Unsupported Arch/OS: x86_64/{unknown}")),
        },
        "aarch64" => match env::consts::OS {
            "linux" => "aarch64-unknown-linux-gnu",
            "darwin" | "macos" => "aarch64-apple-darwin",
            unknown => return Err(anyhow!("Unsupported Arch/OS: aarch64/{unknown}")),
        },
        arch => return Err(anyhow!("Unsupported Architecture: {arch}")),
    })
}

fn download_http(version: String, output_dir: &Path) -> Result<PathBuf, Error> {
    let releases = get_releases(output_dir)?;
    let mut dir_path = output_dir.to_path_buf();

    let mut artifact = None;
    let arch = get_arch()?;

    for release in releases {
        if release.name.as_ref() == Some(&version) {
            if release.tag_name == version {
                dir_path.push(release.target_commitish);
            }
            for asset in release.assets {
                let name = asset.name;
                let url = asset.browser_download_url;
                if name == format!("buck2-{}.zst", arch) {
                    artifact = Some((name, url));
                    break;
                }
            }
        }
    }

    let (name, url) = if let Some(artifact) = artifact {
        artifact
    } else {
        return Err(anyhow!("{version} was not available. Please check '{BUCK_RELEASE_URL}' for available releases."));
    };

    let binary_path: PathBuf = [&dir_path, Path::new("buck2")].iter().collect();
    if binary_path.exists() {
        // unpacked binary already present, so do nothing
        return Ok(dir_path);
    }

    // Create the release directory if it doesn't exist
    fs::create_dir_all(&dir_path).with_context(|| anyhow!("problem creating {:?}", dir_path))?;

    {
        // Fetch the prelude hash and store it, do this before the binary so we don't see a partial hash
        // We do this as the complete executable is atomic via tmp_file rename
        let prelude_path: PathBuf = [&dir_path, Path::new("prelude_hash")].iter().collect();
        let resp = reqwest::blocking::get(format!("{BASE_URL}/{version}/prelude_hash"))?;
        let mut prelude_hash = File::create(prelude_path)?;
        prelude_hash.write_all(&resp.bytes()?)?;
        prelude_hash.flush()?;
    }

    // Fetch the buck2 archive, decode it, make it executable
    let mut tmp_file = NamedTempFile::new_in(&dir_path)?;
    eprintln!("buckle: fetching {name} {version}");
    let resp = reqwest::blocking::get(url)?;
    zstd::stream::copy_decode(resp, &tmp_file)?;
    tmp_file.flush()?;
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&tmp_file, permissions)
            .with_context(|| anyhow!("problem setting permissions on {:?}", tmp_file))?;
    }
    // only move to final binary_path once fully written and stable
    fs::rename(tmp_file.path(), &binary_path)
        .with_context(|| anyhow!("problem renaming {:?} to {:?}", tmp_file, binary_path))?;

    Ok(dir_path)
}

fn get_expected_prelude_hash() -> &'static str {
    static INSTANCE: OnceCell<String> = OnceCell::new();
    let expected_hash = INSTANCE.get_or_init(|| {
        let mut prelude_hash_path = get_buck2_dir().unwrap();
        prelude_hash_path.push("prelude_hash");

        let mut prelude_hash = File::open(prelude_hash_path).unwrap();
        let mut buf = vec![];
        prelude_hash.read_to_end(&mut buf).unwrap();
        std::str::from_utf8(&buf)
            .unwrap()
            .to_string()
            .trim()
            .to_string()
    });
    expected_hash
}

fn read_buck2_version() -> Result<String, Error> {
    if let Ok(version) = env::var("USE_BUCK2_VERSION") {
        return Ok(version);
    }

    if let Some(root) = get_buck2_project_root() {
        let root: PathBuf = [root, Path::new(".buckversion")].iter().collect();
        if root.exists() {
            return Ok(fs::read_to_string(root)?.trim().to_string());
        }
    }

    Ok(String::from("latest"))
}

fn get_buck2_dir() -> Result<PathBuf, Error> {
    let buckle_dir = get_buckle_dir()?.join("buck2");
    if !buckle_dir.exists() {
        fs::create_dir_all(&buckle_dir)?;
    }

    let buck2_version = read_buck2_version()?;
    download_http(buck2_version, &buckle_dir)
}

// Warn if the prelude does not match expected
fn verify_prelude(prelude_path: &str) {
    if let Some(absolute_prelude_path) = get_buck2_project_root() {
        let mut absolute_prelude_path = absolute_prelude_path.to_path_buf();
        absolute_prelude_path.push(prelude_path);
        // It's ok if it's not a git repo, but we don't have support
        // for checking other methods yet. Do not throw an error.
        if let Ok(repo) = git2::Repository::open_from_env() {
            // It makes no sense for buck2 to be invoked on a bare git repo.
            let git_workdir = repo.workdir().expect("buck2 is not for bare git repos");
            let git_relative_prelude_path = absolute_prelude_path
                .strip_prefix(git_workdir)
                .expect("buck2 prelude is not in the same git repo")
                .to_str()
                .unwrap();
            // If there is a prelude known
            if let Ok(prelude) = repo.find_submodule(git_relative_prelude_path) {
                // Don't check if there is no ID.
                if let Some(prelude_hash) = prelude.workdir_id() {
                    let prelude_hash = prelude_hash.to_string();
                    let expected_hash = get_expected_prelude_hash();
                    if prelude_hash != expected_hash {
                        mismatched_prelude_msg(&absolute_prelude_path, &prelude_hash, expected_hash)
                    }
                }
            }
        }
    }
}

/// Notify user of prelude mismatch and suggest solution.
// TODO make this much better
fn mismatched_prelude_msg(absolute_prelude_path: &Path, prelude_hash: &str, expected_hash: &str) {
    eprintln!(
        "buckle: Git submodule for prelude ({prelude_hash}) is not the expected {expected_hash}."
    );
    let abs_path = absolute_prelude_path.display();
    eprintln!("buckle: cd {abs_path} && git fetch && git checkout {expected_hash}");
}

fn main() -> Result<(), Error> {
    let buck2_path: PathBuf = [get_buck2_dir()?, PathBuf::from("buck2")].iter().collect();
    if !buck2_path.exists() {
        return Err(anyhow!(
            "The buckle cache is corrupted. Suggested fix is to remove {}",
            get_buckle_dir()?.display()
        ));
    }

    // mode() is only available on unix systems
    #[cfg(unix)]
    if buck2_path.exists() {
        let metadata = buck2_path.metadata()?;
        let permissions = metadata.permissions();
        let is_exec = metadata.is_file() && permissions.mode() & 0o111 != 0;
        if !is_exec {
            return Err(anyhow!(
                "The buckle cache is corrupted. Suggested fix is to remove {}",
                get_buckle_dir()?.display()
            ));
        }
    }

    if env::var("BUCKLE_PRELUDE_CHECK")
        .map(|var| var != "NO")
        .unwrap_or(true)
    {
        // If we can't find the project root, just skip checking the prelude and call the buck2 binary
        if let Some(root) = get_buck2_project_root() {
            // If we fail to parse the ini file, don't throw an error. We can't parse it for
            // some reason, so we should fall back on buck2 to throw a better error.
            let buck2config: PathBuf = [root, Path::new(".buckconfig")].iter().collect();
            if let Ok(ini) = Ini::load_from_file(buck2config) {
                if let Some(repos) = ini.section(Some("repositories")) {
                    if let Some(prelude_path) = repos.get("prelude") {
                        verify_prelude(prelude_path);
                    }
                }
            }
        }
    }

    // Collect information indented for buck2 binary.
    let mut args = env::args_os();
    args.next(); // Skip buckle
    let envs = env::vars_os();

    // Pass all file descriptors through as well.
    let status = Command::new(&buck2_path)
        .args(args)
        .envs(envs)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|_| panic!("Failed to execute {}", &buck2_path.display()))
        .status;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
