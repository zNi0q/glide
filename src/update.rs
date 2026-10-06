use std::{
    env, fmt, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{self, Command},
};

use anyhow::{Context, Result, bail};

const REPO: &str = "zNi0q/glide";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let mut next = || parts.next()?.parse().ok();
        Some(Self(next()?, next()?, next()?))
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the package version is valid")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    tag: String,
}

pub fn available() -> Result<Option<Release>> {
    let release = latest()?;
    Ok((release.version > Version::current()).then_some(release))
}

pub fn install(release: &Release) -> Result<PathBuf> {
    let arch = match env::consts::ARCH {
        arch @ ("x86_64" | "aarch64") => arch,
        other => bail!("no release is published for {other} machines"),
    };
    let name = format!("glide-{}-{arch}-linux", release.tag);
    let work = env::temp_dir().join(format!("glide-update-{}", process::id()));
    fs::create_dir_all(&work)?;
    let result = download_and_replace(release, &name, &work);
    let _ = fs::remove_dir_all(&work);
    result
}

fn latest() -> Result<Release> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
        ])
        .arg(format!("https://github.com/{REPO}/releases/latest"))
        .output()
        .context("cannot run curl")?;
    if !out.status.success() {
        bail!("cannot reach GitHub to check for updates");
    }
    let tag = tag_from_url(&String::from_utf8_lossy(&out.stdout))
        .context("no glide release has been published yet")?
        .to_owned();
    let version = Version::parse(&tag).context("the latest release has an unexpected version")?;
    Ok(Release { version, tag })
}

fn tag_from_url(url: &str) -> Option<&str> {
    let tag = url.trim().rsplit_once("/releases/tag/")?.1;
    Version::parse(tag).map(|_| tag)
}

fn download_and_replace(release: &Release, name: &str, work: &Path) -> Result<PathBuf> {
    let base = format!(
        "https://github.com/{REPO}/releases/download/{}",
        release.tag
    );
    for file in [format!("{name}.tar.gz"), format!("{name}.tar.gz.sha256")] {
        run(
            Command::new("curl")
                .args(["-fsSL", "--max-time", "300", "-o"])
                .arg(work.join(&file))
                .arg(format!("{base}/{file}")),
            "download the update",
        )?;
    }
    run(
        Command::new("sha256sum")
            .arg("-c")
            .arg(format!("{name}.tar.gz.sha256"))
            .current_dir(work),
        "verify the download",
    )?;
    run(
        Command::new("tar")
            .arg("-xzf")
            .arg(format!("{name}.tar.gz"))
            .current_dir(work),
        "unpack the update",
    )?;

    let exe = env::current_exe().context("cannot find the running glide binary")?;
    let staged = exe.with_extension("new");
    fs::copy(work.join(name).join("glide"), &staged)
        .with_context(|| format!("cannot write next to {}", exe.display()))?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    fs::rename(&staged, &exe).with_context(|| format!("cannot replace {}", exe.display()))?;
    Ok(exe)
}

fn run(command: &mut Command, action: &str) -> Result<()> {
    let out = command
        .output()
        .with_context(|| format!("cannot {action}"))?;
    if !out.status.success() {
        bail!(
            "cannot {action}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(Version::parse("v0.10.0") > Version::parse("0.9.3"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("latest"), None);
    }

    #[test]
    fn reads_the_tag_from_the_release_redirect() {
        let url = "https://github.com/zNi0q/glide/releases/tag/v0.2.0";
        assert_eq!(tag_from_url(url), Some("v0.2.0"));
        assert_eq!(
            tag_from_url("https://github.com/zNi0q/glide/releases"),
            None
        );
    }
}
