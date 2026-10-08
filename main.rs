use clap::{Parser, Subcommand};
use dialoguer::{Confirm, Input};
use err::{Err, Result, ResultExt};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, process::Stdio};
use tokio::{fs, process::Command};

#[derive(Parser)]
#[command(
    name = "packpub",
    version,
    about = "Publish binary packages to Homebrew and the AUR"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Configure publishing destinations and package metadata
    Setup,
    /// Publish publicly accessible binary URLs
    Publish {
        /// Package name
        name: String,
        /// Release version (for example 1.2.3)
        #[arg(long)]
        version: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        homepage: Option<String>,
        #[arg(long)]
        license: Option<String>,
        #[arg(long = "linux-x86-64")]
        linux_x86_64: Option<String>,
        #[arg(long = "linux-aarch64")]
        linux_aarch64: Option<String>,
        #[arg(long = "macos-x86-64")]
        macos_x86_64: Option<String>,
        #[arg(long = "macos-aarch64")]
        macos_aarch64: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct Config {
    homebrew_tap: String,
    aur_bin_suffix: bool,
    description: String,
    homepage: String,
    license: String,
}

#[derive(Clone)]
struct Artifact {
    platform: &'static str,
    arch: &'static str,
    url: String,
    sha256: String,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:?}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Setup => setup(),
        Commands::Publish {
            name,
            version,
            description,
            homepage,
            license,
            linux_x86_64,
            linux_aarch64,
            macos_x86_64,
            macos_aarch64,
        } => {
            publish(
                &name,
                &version,
                description,
                homepage,
                license,
                [linux_x86_64, linux_aarch64, macos_x86_64, macos_aarch64],
            )
            .await
        }
    }
}

fn setup() -> Result<()> {
    let tap: String = Input::new()
        .with_prompt("Homebrew tap (owner/repo or Git URL)")
        .interact_text()
        .wrap("reading Homebrew tap")?;
    let suffix = Confirm::new()
        .with_prompt("Suffix AUR package names with -bin?")
        .default(true)
        .interact()
        .wrap("reading AUR suffix preference")?;
    let description: String = Input::new()
        .with_prompt("Package description")
        .interact_text()
        .wrap("reading description")?;
    let homepage: String = Input::new()
        .with_prompt("Project homepage URL")
        .allow_empty(true)
        .interact_text()
        .wrap("reading homepage")?;
    let license: String = Input::new()
        .with_prompt("SPDX license identifier")
        .default("MIT".into())
        .interact_text()
        .wrap("reading license")?;

    let config = Config {
        homebrew_tap: tap,
        aur_bin_suffix: suffix,
        description,
        homepage,
        license,
    };
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).wrap("creating config directory")?;
    }
    let contents = toml::to_string_pretty(&config).wrap("serializing config")?;
    std::fs::write(&path, contents).wrap("writing config")?;
    println!("Saved configuration to {}", path.display());
    Ok(())
}

async fn publish(
    name: &str,
    version: &str,
    description: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    urls: [Option<String>; 4],
) -> Result<()> {
    validate_name(name)?;
    validate_version(version)?;
    let config = load_config()?;
    let mut artifacts = Vec::new();
    let targets = [
        ("linux-x86-64", "x86_64", urls[0].clone()),
        ("linux-aarch64", "aarch64", urls[1].clone()),
        ("macos-x86-64", "x86_64", urls[2].clone()),
        ("macos-aarch64", "aarch64", urls[3].clone()),
    ];
    for (platform, arch, url) in targets {
        if let Some(url) = url {
            validate_url(&url)?;
            println!("Fetching SHA-256: {url}");
            let sha256 = hash_url(&url).await.wrap("hashing artifact")?;
            artifacts.push(Artifact {
                platform,
                arch,
                url,
                sha256,
            });
        }
    }
    if artifacts.is_empty() {
        return Err(Err::new("provide at least one platform URL"));
    }

    let aur_artifacts: Vec<_> = artifacts
        .iter()
        .filter(|artifact| artifact.platform.starts_with("linux-"))
        .cloned()
        .collect();
    if aur_artifacts.is_empty() {
        return Err(Err::new(
            "provide at least one Linux URL for the AUR package",
        ));
    }

    let desc = description.as_deref().unwrap_or(&config.description);
    let home = homepage.as_deref().unwrap_or(&config.homepage);
    let lic = license.as_deref().unwrap_or(&config.license);
    let formula = homebrew_formula(name, version, desc, home, lic, &artifacts);
    let tap = git_url(&config.homebrew_tap);
    let dir = tempfile::tempdir().wrap("creating temporary Homebrew tap directory")?;
    git(
        &[
            "clone",
            &tap,
            dir.path()
                .to_str()
                .ok_or_else(|| Err::new("invalid temporary path"))?,
        ],
        None,
    )
    .await
    .wrap("cloning Homebrew tap")?;
    let formula_dir = dir.path().join("Formula");
    fs::create_dir_all(&formula_dir)
        .await
        .wrap("creating Homebrew Formula directory")?;
    fs::write(formula_dir.join(format!("{name}.rb")), formula)
        .await
        .wrap("writing Homebrew formula")?;
    commit_and_push(dir.path(), &format!("Update {name} to {version}"))
        .await
        .wrap("publishing Homebrew formula")?;

    let aur_name = if config.aur_bin_suffix && !name.ends_with("-bin") {
        format!("{name}-bin")
    } else {
        name.to_owned()
    };
    let aur_dir = tempfile::tempdir().wrap("creating temporary AUR directory")?;
    let aur_git = format!("ssh://aur@aur.archlinux.org/{aur_name}.git");
    git(
        &[
            "clone",
            &aur_git,
            aur_dir
                .path()
                .to_str()
                .ok_or_else(|| Err::new("invalid temporary path"))?,
        ],
        None,
    )
    .await
    .wrap("cloning AUR repository")?;
    let pkgbuild = aur_pkgbuild(name, &aur_name, version, desc, home, lic, &aur_artifacts);
    fs::write(aur_dir.path().join("PKGBUILD"), pkgbuild)
        .await
        .wrap("writing PKGBUILD")?;
    commit_and_push(aur_dir.path(), &format!("Update {aur_name} to {version}"))
        .await
        .wrap("publishing AUR package")?;
    Ok(())
}

async fn hash_url(url: &str) -> Result<String> {
    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .wrap("requesting public artifact URL")?;
    if !response.status().is_success() {
        return Err(Err::from_error(format!(
            "artifact URL returned HTTP {}: {url}",
            response.status()
        )));
    }
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        hasher.update(chunk.wrap("reading artifact response")?);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn homebrew_formula(
    name: &str,
    version: &str,
    description: &str,
    homepage: &str,
    license: &str,
    artifacts: &[Artifact],
) -> String {
    let mut out = format!(
        "class {} < Formula\n  desc \"{}\"\n",
        ruby_const(name),
        ruby_escape(description)
    );
    if !homepage.is_empty() {
        out.push_str(&format!("  homepage \"{}\"\n", ruby_escape(homepage)));
    }
    out.push_str(&format!(
        "  version \"{}\"\n  license \"{}\"\n\n",
        ruby_escape(version),
        ruby_escape(license)
    ));
    out.push_str("  on_macos do\n");
    append_brew_arch(&mut out, artifacts, "macos-aarch64", "arm");
    append_brew_arch(&mut out, artifacts, "macos-x86-64", "intel");
    out.push_str("  end\n  on_linux do\n");
    append_brew_arch(&mut out, artifacts, "linux-aarch64", "arm");
    append_brew_arch(&mut out, artifacts, "linux-x86-64", "intel");
    out.push_str("  end\n\n  def install\n    bin.install Dir[\"*\"]\n  end\nend\n");
    out
}

fn append_brew_arch(out: &mut String, artifacts: &[Artifact], platform: &str, arch_kind: &str) {
    if let Some(a) = artifacts.iter().find(|a| a.platform == platform) {
        out.push_str(&format!(
            "    on_{arch_kind} do\n      url \"{}\"\n      sha256 \"{}\"\n    end\n",
            ruby_escape(&a.url),
            a.sha256
        ));
    }
}

fn aur_pkgbuild(
    name: &str,
    aur_name: &str,
    version: &str,
    description: &str,
    homepage: &str,
    license: &str,
    artifacts: &[Artifact],
) -> String {
    let mut out = format!(
        "# Maintained by packpub\npkgname={}\npkgver={}\npkgrel=1\npkgdesc=\"{}\"\nurl=\"{}\"\nlicense=('{}')\narch=(",
        aur_name,
        version.replace('-', "_"),
        sh_double_escape(description),
        sh_double_escape(homepage),
        sh_single_escape(license)
    );
    let mut arches: Vec<_> = artifacts.iter().map(|a| a.arch).collect();
    arches.sort_unstable();
    arches.dedup();
    out.push_str(
        &arches
            .iter()
            .map(|a| format!("'{}'", a))
            .collect::<Vec<_>>()
            .join(" "),
    );
    out.push_str(")\nmakedepends=('unzip')\nsource=(\n");
    for a in artifacts {
        out.push_str(&format!(
            "  '{}::{}'\n",
            a.platform,
            sh_single_escape(&a.url)
        ));
    }
    out.push_str(")\nnoextract=(");
    out.push_str(
        &artifacts
            .iter()
            .map(|a| format!("'{}'", a.platform))
            .collect::<Vec<_>>()
            .join(" "),
    );
    out.push_str(")\nsha256sums=(\n");
    for a in artifacts {
        out.push_str(&format!("  '{}'\n", a.sha256));
    }
    out.push_str(")\n\npackage() {\n  local artifact\n  case \"$CARCH\" in\n");
    for a in artifacts {
        out.push_str(&format!(
            "    '{}' ) artifact=\"$srcdir/{}\" ;;\n",
            a.arch, a.platform
        ));
    }
    out.push_str("    *) return 1 ;;\n  esac\n  if unzip -tq \"$artifact\" >/dev/null 2>&1; then\n    unzip -p \"$artifact\" '");
    out.push_str(name);
    out.push_str("' > \"$srcdir/");
    out.push_str(name);
    out.push_str("\"\n    install -Dm755 \"$srcdir/");
    out.push_str(name);
    out.push_str("\" \"$pkgdir/usr/bin/");
    out.push_str(name);
    out.push_str("\"\n  else\n    install -Dm755 \"$artifact\" \"$pkgdir/usr/bin/");
    out.push_str(name);
    out.push_str("\"\n  fi\n}\n");
    out
}

async fn commit_and_push(dir: &std::path::Path, message: &str) -> Result<()> {
    git(&["add", "-A"], Some(dir)).await?;
    let status = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(dir)
        .status()
        .await
        .wrap("checking for staged changes")?;
    if status.success() {
        return Ok(());
    }
    if status.code() != Some(1) {
        return Err(Err::new("git diff --cached failed"));
    }
    git(&["commit", "-m", message], Some(dir)).await?;
    git(&["push"], Some(dir)).await
}

async fn git(args: &[&str], cwd: Option<&std::path::Path>) -> Result<()> {
    let mut command = Command::new("git");
    command
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let status = command.status().await.wrap("running git command")?;
    if !status.success() {
        return Err(Err::from_error(format!(
            "git {} exited with {status}",
            args.join(" ")
        )));
    }
    Ok(())
}

fn load_config() -> Result<Config> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).wrap("reading config (run `packpub setup` first)")?;
    toml::from_str(&text).wrap("parsing config")
}

fn config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| Err::new("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/packpub/config.toml"))
}

fn git_url(tap: &str) -> String {
    if tap.contains("://") || tap.starts_with("git@") {
        return tap.to_owned();
    }
    let (owner, repo) = tap.split_once('/').unwrap_or((tap, "tap"));
    let repo = if repo.starts_with("homebrew-") {
        repo.to_owned()
    } else {
        format!("homebrew-{repo}")
    };
    format!("https://github.com/{owner}/{repo}.git")
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Err::new(
            "package name may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}
fn validate_version(version: &str) -> Result<()> {
    if version.is_empty()
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
    {
        return Err(Err::new("invalid version string"));
    }
    Ok(())
}
fn validate_url(url: &str) -> Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Err::from_error(format!(
            "URL must be public HTTP(S): {url}"
        )));
    }
    Ok(())
}
fn ruby_const(name: &str) -> String {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|x| x.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}
fn ruby_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}
fn sh_single_escape(value: &str) -> String {
    value.replace('\'', "'\\''").replace('\n', " ")
}

fn sh_double_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
        .replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::git_url;

    #[test]
    fn homebrew_tap_short_name_expands_to_repository_name() {
        assert_eq!(
            git_url("vehmloewff/tap"),
            "https://github.com/vehmloewff/homebrew-tap.git"
        );
    }

    #[test]
    fn explicit_homebrew_repo_is_preserved() {
        assert_eq!(
            git_url("vehmloewff/homebrew-tap"),
            "https://github.com/vehmloewff/homebrew-tap.git"
        );
    }
}
