use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const STACK_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "stack.json";
const DEFAULT_PROFILE: &str = "default";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StackManifest {
    version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    omp_profiles: Vec<OmpProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skillshare: Option<SkillshareSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OmpProfile {
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    plugins: Vec<PortablePlugin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mcp: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required_env: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortablePlugin {
    name: String,
    source: PluginSource,
    enabled: bool,
    enabled_features: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum PluginSource {
    Package { spec: String },
    Git { url: String, rev: String },
    Snapshot { path: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillshareSource {
    remote: String,
    git_root: String,
}

pub fn capture(output: Option<&Path>) -> io::Result<()> {
    let output = match output {
        Some(output) => output.to_path_buf(),
        None => default_stack_dir()?,
    };
    let manifest = capture_into(&output)?;
    let plugin_count: usize = manifest
        .omp_profiles
        .iter()
        .map(|profile| profile.plugins.len())
        .sum();
    println!(
        "Captured {} OMP profile(s), {plugin_count} plugin(s) to {}",
        manifest.omp_profiles.len(),
        output.display()
    );
    if manifest.skillshare.is_none() {
        println!("Skillshare has no Git remote; skills remain local.");
    }
    Ok(())
}

pub fn restore(
    input: Option<&Path>,
    remote: Option<&str>,
    git_root: &str,
    trust: bool,
    force: bool,
) -> io::Result<()> {
    if let Some(remote) = remote {
        let already_initialized = skillshare_repository()?.is_some();
        ensure_skillshare_repository(remote, git_root)?;
        if !already_initialized {
            let stack = default_stack_dir()?;
            println!("Cloned Skillshare stack source.");
            println!(
                "Review {} and local-plugins/ before restoring.",
                stack.display()
            );
            println!("Then run: synapse stack restore --trust");
            return Ok(());
        }
    }
    if !trust {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stack restore installs executable plugins and MCP commands; review the capture and pass --trust",
        ));
    }
    let input = match input {
        Some(input) => input.to_path_buf(),
        None => default_stack_dir()?,
    };
    let manifest = read_manifest(&input)?;
    validate_manifest(&manifest)?;
    validate_skillshare_preflight(&manifest)?;
    restore_from(&input, &manifest, force)?;
    println!("Restored AI stack from {}", input.display());
    Ok(())
}

pub fn status(input: Option<&Path>) -> io::Result<()> {
    let input = match input {
        Some(input) => input.to_path_buf(),
        None => default_stack_dir()?,
    };
    let manifest = read_manifest(&input)?;
    validate_manifest(&manifest)?;

    let mut missing = BTreeSet::new();
    for profile in &manifest.omp_profiles {
        for name in &profile.required_env {
            if env::var_os(name).map_or(true, |value| value.is_empty()) {
                missing.insert(name);
            }
        }
    }
    println!("Stack: {}", input.display());
    println!("OMP profiles: {}", manifest.omp_profiles.len());
    println!(
        "Plugins: {}",
        manifest
            .omp_profiles
            .iter()
            .map(|profile| profile.plugins.len())
            .sum::<usize>()
    );
    println!(
        "Skillshare: {}",
        manifest
            .skillshare
            .as_ref()
            .map(|source| source.remote.as_str())
            .unwrap_or("no Git remote")
    );
    if missing.is_empty() {
        println!("Required environment: ready");
    } else {
        println!(
            "Required environment: missing {}",
            missing.into_iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn default_stack_dir() -> io::Result<PathBuf> {
    skillshare_repository()?
        .map(|(repo, _)| repo.join(".synapse").join("stack"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Skillshare Git remote not configured; pass an explicit tracked stack path",
            )
        })
}

fn capture_into(output: &Path) -> io::Result<StackManifest> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_input("stack output must have a file name"))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir(&staging)?;

    let result = (|| {
        let omp_root = omp_root()?;
        let mut profiles = vec![(DEFAULT_PROFILE.to_string(), omp_root.clone())];
        let profiles_dir = omp_root.join("profiles");
        if profiles_dir.is_dir() {
            let mut entries: Vec<_> = fs::read_dir(&profiles_dir)?.collect::<Result<_, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if entry.file_type()?.is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if valid_profile_name(&name) {
                        profiles.push((name, entry.path()));
                    }
                }
            }
        }

        let mut omp_profiles = Vec::new();
        for (name, root) in profiles {
            let profile = capture_profile(&name, &root, &staging)?;
            if !profile.plugins.is_empty() || profile.mcp.is_some() {
                omp_profiles.push(profile);
            }
        }
        let manifest = StackManifest {
            version: STACK_VERSION,
            omp_profiles,
            skillshare: capture_skillshare()?,
        };
        write_json(&staging.join(MANIFEST_FILE), &manifest)?;
        Ok(manifest)
    })();

    match result {
        Ok(manifest) => {
            if output.exists() {
                if !output.is_dir() {
                    fs::remove_dir_all(&staging).ok();
                    return Err(invalid_input(format!(
                        "stack output {} is not a directory",
                        output.display()
                    )));
                }
                let previous = parent.join(format!(".{name}.previous-{}", std::process::id()));
                if previous.exists() {
                    fs::remove_dir_all(&previous)?;
                }
                fs::rename(output, &previous)?;
                if let Err(error) = fs::rename(&staging, output) {
                    fs::rename(&previous, output).ok();
                    return Err(error);
                }
                fs::remove_dir_all(previous)?;
            } else {
                fs::rename(&staging, output)?;
            }
            Ok(manifest)
        }
        Err(error) => {
            fs::remove_dir_all(&staging).ok();
            Err(error)
        }
    }
}

fn capture_profile(name: &str, root: &Path, staging: &Path) -> io::Result<OmpProfile> {
    let plugins_dir = root.join("plugins");
    let package = read_json_optional(&plugins_dir.join("package.json"))?;
    let lock = read_json_optional(&plugins_dir.join("omp-plugins.lock.json"))?;
    let dependencies = string_map(package.as_ref().and_then(|value| value.get("dependencies")))?;
    let lock_plugins = lock
        .as_ref()
        .and_then(|value| value.get("plugins"))
        .and_then(Value::as_object);
    let mut names: BTreeSet<String> = dependencies.keys().cloned().collect();
    if let Some(plugins) = lock_plugins {
        names.extend(plugins.keys().cloned());
    }

    let mut plugins = Vec::new();
    for plugin_name in names {
        validate_plugin_name(&plugin_name)?;
        let state = lock_plugins.and_then(|plugins| plugins.get(&plugin_name));
        let enabled = state
            .and_then(|state| state.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let enabled_features = match state.and_then(|state| state.get("enabledFeatures")) {
            Some(Value::Array(features)) => Some(
                features
                    .iter()
                    .map(|feature| {
                        feature.as_str().map(str::to_string).ok_or_else(|| {
                            invalid_data(format!(
                                "plugin {plugin_name:?} enabledFeatures must contain strings"
                            ))
                        })
                    })
                    .collect::<io::Result<Vec<_>>>()?,
            ),
            Some(Value::Null) | None => None,
            Some(_) => {
                return Err(invalid_data(format!(
                    "plugin {plugin_name:?} enabledFeatures must be an array or null"
                )))
            }
        };
        let source = capture_plugin_source(
            name,
            &plugin_name,
            dependencies.get(&plugin_name).map(String::as_str),
            state
                .and_then(|state| state.get("version"))
                .and_then(Value::as_str),
            &plugins_dir,
            staging,
        )?;
        plugins.push(PortablePlugin {
            name: plugin_name,
            source,
            enabled,
            enabled_features,
        });
    }

    let mcp_path = root.join("agent").join("mcp.json");
    let (mcp, required_env) = match read_json_optional(&mcp_path)? {
        Some(value) => {
            let (portable, required_env) = portable_mcp(&value)?;
            (Some(portable), required_env)
        }
        None => (None, Vec::new()),
    };

    Ok(OmpProfile {
        name: name.to_string(),
        plugins,
        mcp,
        required_env,
    })
}

fn capture_plugin_source(
    profile: &str,
    name: &str,
    dependency: Option<&str>,
    installed_version: Option<&str>,
    plugins_dir: &Path,
    staging: &Path,
) -> io::Result<PluginSource> {
    if let Some((url, rev)) = dependency.and_then(pinned_git_dependency) {
        return Ok(PluginSource::Git { url, rev });
    }
    if let Some(spec) = dependency.filter(|spec| registry_dependency(spec)) {
        let version = installed_version
            .filter(|version| exact_registry_version(version))
            .map(str::to_string)
            .or_else(|| installed_package_version(plugins_dir, name).ok().flatten())
            .ok_or_else(|| {
                invalid_data(format!(
                    "plugin {name:?} has no exact installed registry version"
                ))
            })?;
        let spec = npm_restore_spec(name, spec, &version)?;
        return Ok(PluginSource::Package { spec });
    }

    let source = dependency
        .and_then(local_dependency_path)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                plugins_dir.join(path)
            }
        })
        .unwrap_or_else(|| plugins_dir.join("node_modules").join(name));
    let source = fs::canonicalize(&source).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot resolve local plugin {name:?}: {error}"),
        )
    })?;

    if let Some((url, rev)) = clean_git_source(&source) {
        return Ok(PluginSource::Git { url, rev });
    }
    ensure_snapshot_reproducible(&source)?;

    let relative = PathBuf::from("local-plugins")
        .join(path_component(profile))
        .join(path_component(name));
    copy_tree(&source, &staging.join(&relative), &source)?;
    Ok(PluginSource::Snapshot {
        path: relative.to_string_lossy().replace('\\', "/"),
    })
}

fn ensure_snapshot_reproducible(source: &Path) -> io::Result<()> {
    if !source.is_dir() {
        return Err(invalid_data(format!(
            "local plugin {} is not a directory and cannot be snapshotted",
            source.display()
        )));
    }
    if snapshot_has_dependencies(source)?
        && !source.join("bun.lock").is_file()
        && !source.join("bun.lockb").is_file()
    {
        return Err(invalid_data(format!(
            "local plugin {} has dependencies but no Bun lockfile",
            source.display()
        )));
    }
    Ok(())
}

fn snapshot_has_dependencies(path: &Path) -> io::Result<bool> {
    let Some(package) = read_json_optional(&path.join("package.json"))? else {
        return Ok(false);
    };
    Ok([
        "dependencies",
        "optionalDependencies",
        "devDependencies",
        "peerDependencies",
    ]
    .iter()
    .filter_map(|field| package.get(field))
    .any(|value| {
        value
            .as_object()
            .is_some_and(|dependencies| !dependencies.is_empty())
    }))
}

fn installed_package_version(plugins_dir: &Path, name: &str) -> io::Result<Option<String>> {
    let package = read_json_optional(
        &plugins_dir
            .join("node_modules")
            .join(name)
            .join("package.json"),
    )?;
    let version = package
        .as_ref()
        .and_then(|package| package.get("version"))
        .and_then(Value::as_str);
    match version {
        Some(version) if exact_registry_version(version) => Ok(Some(version.to_string())),
        Some(_) => Err(invalid_data(format!(
            "plugin {name:?} has a non-exact installed version"
        ))),
        None => Ok(None),
    }
}

fn exact_registry_version(version: &str) -> bool {
    if version.is_empty() || version.len() > 128 {
        return false;
    }
    let (without_build, build) = version
        .split_once('+')
        .map_or((version, None), |(core, build)| (core, Some(build)));
    if build.is_some_and(|value| !valid_semver_identifiers(value)) {
        return false;
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    if prerelease.is_some_and(|value| !valid_semver_identifiers(value)) {
        return false;
    }
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn valid_semver_identifiers(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn npm_restore_spec(name: &str, dependency: &str, version: &str) -> io::Result<String> {
    let Some(alias) = dependency.strip_prefix("npm:") else {
        return Ok(format!("{name}@{version}"));
    };
    let target = if let Some(scoped) = alias.strip_prefix('@') {
        scoped
            .rfind('@')
            .map(|index| &alias[..index + 1])
            .unwrap_or(alias)
    } else {
        alias
            .rsplit_once('@')
            .map(|(target, _)| target)
            .unwrap_or(alias)
    };
    validate_plugin_name(target)?;
    Ok(format!("{name}@npm:{target}@{version}"))
}

fn validate_package_source(name: &str, spec: &str) -> io::Result<()> {
    validate_argument("package spec", spec)?;
    if !valid_npm_package_name(name) {
        return Err(invalid_data(format!("invalid npm package name {name:?}")));
    }
    let rest = spec.strip_prefix(&format!("{name}@")).ok_or_else(|| {
        invalid_data(format!(
            "plugin {name:?} package spec does not install the same package"
        ))
    })?;
    if let Some(alias) = rest.strip_prefix("npm:") {
        let (target, version) = alias
            .rsplit_once('@')
            .ok_or_else(|| invalid_data(format!("plugin {name:?} has malformed npm alias")))?;
        if !valid_npm_package_name(target) || !exact_registry_version(version) {
            return Err(invalid_data(format!(
                "plugin {name:?} npm alias is not pinned to an exact registry package"
            )));
        }
        return Ok(());
    }
    if !exact_registry_version(rest) {
        return Err(invalid_data(format!(
            "plugin {name:?} package spec is not pinned to an exact version"
        )));
    }
    Ok(())
}

fn valid_npm_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if let Some(scoped) = name.strip_prefix('@') {
        let mut parts = scoped.split('/');
        matches!((parts.next(), parts.next(), parts.next()), (Some(scope), Some(package), None) if valid_part(scope) && valid_part(package))
    } else {
        !name.contains(['/', '@']) && valid_part(name)
    }
}

fn pinned_git_dependency(spec: &str) -> Option<(String, String)> {
    let (origin, revision) = spec.rsplit_once('#')?;
    if !valid_revision(revision) {
        return None;
    }
    let origin = normalized_declared_git_origin(origin)?;
    validate_remote(&origin).ok()?;
    Some((origin, revision.to_string()))
}

fn normalized_declared_git_origin(origin: &str) -> Option<String> {
    if declared_git_origin(origin) {
        return Some(origin.to_string());
    }
    let mut parts = origin.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_none()
        && !owner.is_empty()
        && !repo.is_empty()
        && owner
            .chars()
            .chain(repo.chars())
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Some(format!("github:{owner}/{repo}"))
    } else {
        None
    }
}

fn declared_git_origin(origin: &str) -> bool {
    [
        "github:",
        "gitlab:",
        "bitbucket:",
        "codeberg:",
        "sourcehut:",
        "srht:",
        "git:",
        "git+",
        "ssh://",
        "http://",
        "https://",
        "git@",
    ]
    .iter()
    .any(|prefix| origin.starts_with(prefix))
}

fn registry_dependency(spec: &str) -> bool {
    ![
        "file:",
        "link:",
        "workspace:",
        "git:",
        "git+",
        "github:",
        "gitlab:",
        "bitbucket:",
        "codeberg:",
        "sourcehut:",
        "srht:",
        "http://",
        "https://",
        "ssh://",
        "./",
        "../",
        "/",
    ]
    .iter()
    .any(|prefix| spec.starts_with(prefix))
}

fn local_dependency_path(spec: &str) -> Option<PathBuf> {
    ["file:", "link:"]
        .iter()
        .find_map(|prefix| spec.strip_prefix(prefix))
        .map(PathBuf::from)
}

fn clean_git_source(path: &Path) -> Option<(String, String)> {
    let top_level = git_output(path, &["rev-parse", "--show-toplevel"])
        .and_then(|top_level| fs::canonicalize(top_level).ok())?;
    if top_level != path {
        return None;
    }
    let status = git_output(path, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return None;
    }
    let url = git_output(path, &["remote", "get-url", "origin"])?;
    if validate_remote(&url).is_err() {
        return None;
    }
    let rev = git_output(path, &["rev-parse", "HEAD"])?;
    if !valid_revision(&rev) {
        return None;
    }
    let remote_branches = git_output(path, &["branch", "-r", "--contains", &rev])?;
    if !remote_branches
        .lines()
        .map(str::trim)
        .any(|branch| branch.starts_with("origin/"))
    {
        return None;
    }
    Some((url, rev))
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    let git = env::var_os("SYNAPSE_GIT_BIN").unwrap_or_else(|| "git".into());
    let output = Command::new(git)
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn copy_tree(source: &Path, destination: &Path, root: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    let mut entries: Vec<_> = fs::read_dir(source)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_name = entry.file_name();
        if secret_snapshot_entry(&file_name) {
            return Err(invalid_data(format!(
                "local plugin snapshot contains credential-like file {}",
                entry.path().display()
            )));
        }
        if excluded_snapshot_entry(&file_name) {
            continue;
        }

        let from = entry.path();
        let to = destination.join(&file_name);
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&from)?;
            if target.is_absolute() {
                return Err(invalid_data(format!(
                    "plugin symlink {} is absolute and not portable",
                    from.display()
                )));
            }
            let resolved = fs::canonicalize(from.parent().unwrap_or(source).join(&target))?;
            if !resolved.starts_with(root) {
                return Err(invalid_data(format!(
                    "plugin symlink {} escapes its source directory",
                    from.display()
                )));
            }
            let relative_target = resolved.strip_prefix(root).map_err(|_| {
                invalid_data(format!(
                    "plugin symlink {} escapes its source directory",
                    from.display()
                ))
            })?;
            if relative_target
                .components()
                .filter_map(|component| match component {
                    Component::Normal(name) => Some(name),
                    _ => None,
                })
                .any(excluded_snapshot_entry)
            {
                return Err(invalid_data(format!(
                    "plugin symlink {} points into an excluded cache",
                    from.display()
                )));
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, to)?;
            #[cfg(not(unix))]
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local plugin symlinks require a Unix-compatible platform",
            ));
        } else if metadata.is_dir() {
            copy_tree(&from, &to, root)?;
        } else if metadata.is_file() {
            fs::copy(from, to)?;
        }
    }
    Ok(())
}
fn secret_snapshot_entry(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    let lower = name.to_ascii_lowercase();
    (lower == ".env"
        || (lower.starts_with(".env.")
            && !matches!(
                lower.as_str(),
                ".env.example" | ".env.sample" | ".env.template"
            )))
        || matches!(
            lower.as_str(),
            ".npmrc"
                | ".netrc"
                | ".yarnrc.yml"
                | ".pypirc"
                | ".docker"
                | ".aws"
                | "credentials.json"
                | "secrets.json"
                | "id_rsa"
                | "id_ed25519"
                | "service-account.json"
        )
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
}

fn excluded_snapshot_entry(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | "node_modules"
                | "target"
                | ".cache"
                | "__pycache__"
                | ".npm"
                | ".pnpm-store"
                | ".turbo"
                | ".pytest_cache"
        )
    )
}

fn skillshare_repository() -> io::Result<Option<(PathBuf, SkillshareSource)>> {
    let config_path = env::var_os("SYNAPSE_SKILLSHARE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            crate::state::config_dir()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("skillshare")
                .join("config.yaml")
        });
    let text = match fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let config: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|error| invalid_data(error.to_string()))?;
    let git_root = match config.get("git_root") {
        None => "skills".to_string(),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| invalid_data("Skillshare git_root must be a string"))?,
    };
    if !matches!(git_root.as_str(), "root" | "skills" | "agents" | "extras") {
        return Err(invalid_data(format!(
            "unsupported Skillshare git_root {git_root:?}"
        )));
    }
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let repo = match git_root.as_str() {
        "root" => config_dir.to_path_buf(),
        "skills" => {
            yaml_path(&config, "source", config_dir).unwrap_or_else(|| config_dir.join("skills"))
        }
        "agents" => config_dir.join("agents"),
        "extras" => yaml_path(&config, "extras_source", config_dir)
            .unwrap_or_else(|| config_dir.join("extras")),
        _ => unreachable!(),
    };
    let Some(remote) = git_output(&repo, &["remote", "get-url", "origin"]) else {
        return Ok(None);
    };
    validate_remote(&remote)?;
    Ok(Some((repo, SkillshareSource { remote, git_root })))
}

fn capture_skillshare() -> io::Result<Option<SkillshareSource>> {
    Ok(skillshare_repository()?.map(|(_, source)| source))
}

fn yaml_path(config: &serde_yaml::Value, key: &str, base: &Path) -> Option<PathBuf> {
    config
        .get(key)
        .and_then(serde_yaml::Value::as_str)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        })
}

fn read_manifest(input: &Path) -> io::Result<StackManifest> {
    let path = input.join(MANIFEST_FILE);
    let metadata = fs::metadata(&path)?;
    if metadata.len() > 1024 * 1024 {
        return Err(invalid_data("stack manifest exceeds 1 MiB"));
    }
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|error| invalid_data(error.to_string()))
}

fn validate_manifest(manifest: &StackManifest) -> io::Result<()> {
    if manifest.version != STACK_VERSION {
        return Err(invalid_data(format!(
            "unsupported stack version {}; expected {STACK_VERSION}",
            manifest.version
        )));
    }
    let mut profile_names = BTreeSet::new();
    for profile in &manifest.omp_profiles {
        if !valid_profile_name(&profile.name) || !profile_names.insert(&profile.name) {
            return Err(invalid_data(format!(
                "invalid or duplicate OMP profile {:?}",
                profile.name
            )));
        }
        let mut plugin_names = BTreeSet::new();
        for plugin in &profile.plugins {
            validate_plugin_name(&plugin.name)?;
            if !plugin_names.insert(&plugin.name) {
                return Err(invalid_data(format!(
                    "duplicate plugin {:?} in profile {:?}",
                    plugin.name, profile.name
                )));
            }
            match &plugin.source {
                PluginSource::Package { spec } => validate_package_source(&plugin.name, spec)?,
                PluginSource::Git { url, rev } => {
                    validate_remote(url)?;
                    if !declared_git_origin(url) {
                        return Err(invalid_data(format!(
                            "plugin {:?} has an unsupported Git origin",
                            plugin.name
                        )));
                    }
                    if !valid_revision(rev) {
                        return Err(invalid_data(format!(
                            "plugin {:?} has a non-immutable Git revision",
                            plugin.name
                        )));
                    }
                }
                PluginSource::Snapshot { path } => validate_relative_path(Path::new(path))?,
            }
        }
        if let Some(mcp) = &profile.mcp {
            let (portable, required_env) = portable_mcp(mcp)?;
            if &portable != mcp || required_env != profile.required_env {
                return Err(invalid_data(format!(
                    "profile {:?} MCP config is not portable or required_env is stale",
                    profile.name
                )));
            }
        } else if !profile.required_env.is_empty() {
            return Err(invalid_data(format!(
                "profile {:?} has required_env without MCP config",
                profile.name
            )));
        }
    }
    if let Some(skillshare) = &manifest.skillshare {
        validate_remote(&skillshare.remote)?;
        if !matches!(
            skillshare.git_root.as_str(),
            "root" | "skills" | "agents" | "extras"
        ) {
            return Err(invalid_data("invalid Skillshare git_root"));
        }
    }
    Ok(())
}

fn validate_skillshare_preflight(manifest: &StackManifest) -> io::Result<()> {
    let Some(expected) = &manifest.skillshare else {
        return Ok(());
    };
    let Some((_, current)) = skillshare_repository()? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Skillshare Git source is not initialized; pass --remote on a new machine",
        ));
    };
    if current != *expected {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "current Skillshare Git source differs from the captured stack",
        ));
    }
    Ok(())
}

fn restore_from(input: &Path, manifest: &StackManifest, force: bool) -> io::Result<()> {
    let omp_root = omp_root()?;
    for profile in &manifest.omp_profiles {
        let root = profile_root(&omp_root, &profile.name);
        let plugins_dir = root.join("plugins");
        fs::create_dir_all(&plugins_dir)?;
        for plugin in &profile.plugins {
            if !plugin_is_current(&plugins_dir, plugin)? {
                restore_plugin(input, &profile.name, plugin)?;
            }
            apply_plugin_state(&plugins_dir, plugin)?;
        }
        if let Some(mcp) = &profile.mcp {
            restore_mcp(&root.join("agent").join("mcp.json"), mcp, force)?;
        }
    }
    if let Some(skillshare) = &manifest.skillshare {
        restore_skillshare(skillshare)?;
    }
    Ok(())
}

fn plugin_is_current(plugins_dir: &Path, plugin: &PortablePlugin) -> io::Result<bool> {
    let PluginSource::Package { spec } = &plugin.source else {
        return Ok(false);
    };
    let expected = spec
        .rsplit_once('@')
        .map(|(_, version)| version)
        .unwrap_or(spec);
    let lock_current = read_json_optional(&plugins_dir.join("omp-plugins.lock.json"))?
        .and_then(|lock| {
            lock.get("plugins")?
                .get(&plugin.name)?
                .get("version")?
                .as_str()
                .map(|version| version == expected)
        })
        .unwrap_or(false);
    let package_current = installed_package_version(plugins_dir, &plugin.name)?
        .is_some_and(|version| version == expected);
    Ok(lock_current && package_current)
}

fn restore_plugin(input: &Path, profile: &str, plugin: &PortablePlugin) -> io::Result<()> {
    let mut command = omp_command(profile);
    command.arg("plugin");
    match &plugin.source {
        PluginSource::Package { spec } => {
            command.arg("install").arg(spec);
        }
        PluginSource::Git { url, rev } => {
            command.arg("install").arg(format!("{url}#{rev}"));
        }
        PluginSource::Snapshot { path } => {
            let source = input.join(path);
            let source = fs::canonicalize(&source)?;
            let input = fs::canonicalize(input)?;
            if !source.starts_with(&input) {
                return Err(invalid_data(format!(
                    "plugin snapshot {path:?} escapes the stack directory"
                )));
            }
            let destination = crate::state::config_dir()
                .join("restored-plugins")
                .join(path_component(profile))
                .join(path_component(&plugin.name));
            replace_tree(&source, &destination)?;
            install_local_dependencies(&destination, profile)?;
            command.arg("link").arg(destination);
        }
    }
    let status = command.status().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to start OMP while restoring {:?}: {error}",
                plugin.name
            ),
        )
    })?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "OMP failed to restore plugin {:?} for profile {profile:?}",
            plugin.name
        )));
    }
    Ok(())
}

fn install_local_dependencies(path: &Path, profile: &str) -> io::Result<()> {
    if !snapshot_has_dependencies(path)? {
        return Ok(());
    }
    let bun = env::var_os("SYNAPSE_BUN_BIN").unwrap_or_else(|| "bun".into());
    let mut command = Command::new(bun);
    command.arg("install");
    if path.join("bun.lock").is_file() || path.join("bun.lockb").is_file() {
        command.arg("--frozen-lockfile");
    }
    let status = command.current_dir(path).status().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to install local plugin dependencies for profile {profile:?}: {error}"),
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Bun failed to install local plugin dependencies for profile {profile:?}"
        )))
    }
}

fn apply_plugin_state(plugins_dir: &Path, plugin: &PortablePlugin) -> io::Result<()> {
    let path = plugins_dir.join("omp-plugins.lock.json");
    let mut lock = read_json_optional(&path)?.unwrap_or_else(|| {
        serde_json::json!({
            "plugins": {},
            "settings": {}
        })
    });
    let plugins = lock
        .get_mut("plugins")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid_data("OMP plugin lock must contain a plugins object"))?;
    let entry = plugins
        .entry(plugin.name.clone())
        .or_insert_with(|| serde_json::json!({}));
    let entry = entry
        .as_object_mut()
        .ok_or_else(|| invalid_data("OMP plugin lock entry must be an object"))?;
    entry.insert("enabled".into(), Value::Bool(plugin.enabled));
    entry.insert(
        "enabledFeatures".into(),
        plugin
            .enabled_features
            .as_ref()
            .map(|features| Value::Array(features.iter().cloned().map(Value::String).collect()))
            .unwrap_or(Value::Null),
    );

    write_json_value(&path, &lock)
}

fn restore_mcp(path: &Path, portable: &Value, force: bool) -> io::Result<()> {
    let existing = read_json_optional(path)?;
    let merged = merge_mcp(existing.as_ref(), portable, force)?;
    if let Some(existing) = existing {
        if existing != merged {
            let backup = path.with_file_name(format!(
                "{}.synapse-backup",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("mcp.json")
            ));
            if !backup.exists() {
                fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
                fs::copy(path, backup)?;
            }
        }
    }
    write_json_value(path, &merged)
}

fn merge_mcp(existing: Option<&Value>, portable: &Value, force: bool) -> io::Result<Value> {
    let mut merged = existing
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let merged_object = merged
        .as_object_mut()
        .ok_or_else(|| invalid_data("existing MCP config must be an object"))?;
    let portable_object = portable
        .as_object()
        .ok_or_else(|| invalid_data("portable MCP config must be an object"))?;

    let merged_servers = merged_object
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| invalid_data("existing mcpServers must be an object"))?;
    let portable_servers = portable_object
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("portable mcpServers must be an object"))?;
    for (name, server) in portable_servers {
        if let Some(current) = merged_servers.get(name) {
            if current != server && !force {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "MCP server {name:?} already exists with different settings; use --force"
                    ),
                ));
            }
        }
        merged_servers.insert(name.clone(), server.clone());
    }

    for list_name in ["disabledServers", "enabledServers"] {
        if let Some(portable_list) = portable_object.get(list_name).and_then(Value::as_array) {
            let list = merged_object
                .entry(list_name)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| invalid_data(format!("{list_name} must be an array")))?;
            for item in portable_list {
                if !list.contains(item) {
                    list.push(item.clone());
                }
            }
        }
    }
    let disabled = mcp_name_list(portable_object, "disabledServers")?;
    let enabled = mcp_name_list(portable_object, "enabledServers")?;
    if !disabled.is_disjoint(&enabled) {
        return Err(invalid_data(
            "MCP server cannot be both enabled and disabled",
        ));
    }
    if let Some(list) = merged_object
        .get_mut("enabledServers")
        .and_then(Value::as_array_mut)
    {
        list.retain(|item| item.as_str().map_or(true, |name| !disabled.contains(name)));
    }
    if let Some(list) = merged_object
        .get_mut("disabledServers")
        .and_then(Value::as_array_mut)
    {
        list.retain(|item| item.as_str().map_or(true, |name| !enabled.contains(name)));
    }
    if !merged_object.contains_key("$schema") {
        if let Some(schema) = portable_object.get("$schema") {
            merged_object.insert("$schema".into(), schema.clone());
        }
    }
    Ok(merged)
}

fn restore_skillshare(source: &SkillshareSource) -> io::Result<()> {
    ensure_skillshare_repository(&source.remote, &source.git_root)?;
    let binary = env::var_os("SYNAPSE_SKILLSHARE_BIN").unwrap_or_else(|| "skillshare".into());
    let status = Command::new(binary)
        .args(["sync", "--all"])
        .status()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to start Skillshare sync: {error}"),
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("Skillshare sync failed"))
    }
}

fn ensure_skillshare_repository(remote: &str, git_root: &str) -> io::Result<()> {
    validate_remote(remote)?;
    if !matches!(git_root, "root" | "skills" | "agents" | "extras") {
        return Err(invalid_input("invalid Skillshare git_root"));
    }
    if let Some((_, current)) = skillshare_repository()? {
        if current.remote != remote || current.git_root != git_root {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "existing Skillshare Git source differs from the requested stack",
            ));
        }
        return Ok(());
    }

    let binary = env::var_os("SYNAPSE_SKILLSHARE_BIN").unwrap_or_else(|| "skillshare".into());
    let status = Command::new(binary)
        .args([
            "init",
            "--git-root",
            git_root,
            "--remote",
            remote,
            "--all-targets",
            "--no-copy",
            "--no-skill",
        ])
        .status()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to start Skillshare restore: {error}"),
            )
        })?;
    if !status.success() {
        return Err(io::Error::other(
            "Skillshare could not clone or initialize its Git source",
        ));
    }
    match skillshare_repository()? {
        Some((_, current)) if current.remote == remote && current.git_root == git_root => Ok(()),
        _ => Err(io::Error::other(
            "Skillshare initialization did not produce the requested Git source",
        )),
    }
}

fn omp_command(profile: &str) -> Command {
    let binary = env::var_os("SYNAPSE_OMP_BIN").unwrap_or_else(|| "omp".into());
    let mut command = Command::new(binary);
    if profile == DEFAULT_PROFILE {
        command.env_remove("OMP_PROFILE").env_remove("PI_PROFILE");
    } else {
        command.env("OMP_PROFILE", profile);
    }
    command
}

fn omp_root() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("SYNAPSE_OMP_DIR").map(PathBuf::from) {
        return Ok(path);
    }
    if let Some(path) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("omp"))
        .filter(|path| path.exists())
    {
        return Ok(path);
    }
    crate::state::home_dir()
        .map(|home| home.join(".omp"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot resolve OMP data directory"))
}

fn profile_root(omp_root: &Path, profile: &str) -> PathBuf {
    if profile == DEFAULT_PROFILE {
        omp_root.to_path_buf()
    } else {
        omp_root.join("profiles").join(profile)
    }
}

fn replace_tree(source: &Path, destination: &Path) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid_input("restored plugin destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.tmp-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugin"),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    copy_tree(source, &staging, source)?;
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::rename(staging, destination)
}

fn read_json_optional(path: &Path) -> io::Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| invalid_data(error.to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn string_map(value: Option<&Value>) -> io::Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_data("plugin dependencies must be an object"))?;
    object
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
                .ok_or_else(|| invalid_data(format!("plugin dependency {name:?} must be a string")))
        })
        .collect()
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let json = serde_json::to_value(value).map_err(|error| invalid_data(error.to_string()))?;
    write_json_value(path, &json)
}

fn write_json_value(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("JSON destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_input("JSON destination has no file name"))?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| invalid_data(error.to_string()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(temporary, path)
}

fn validate_plugin_name(name: &str) -> io::Result<()> {
    validate_argument("plugin name", name)?;
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data(format!("invalid plugin name {name:?}")));
    }
    Ok(())
}

fn valid_profile_name(name: &str) -> bool {
    name == DEFAULT_PROFILE
        || (!name.is_empty()
            && name != "."
            && name != ".."
            && name.len() <= 100
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_data(format!(
            "stack path {:?} must be relative and cannot traverse",
            path
        )));
    }
    Ok(())
}

fn validate_argument(label: &str, value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.chars().any(|c| c == '\0' || c == '\n' || c == '\r')
    {
        return Err(invalid_data(format!("invalid {label}")));
    }
    Ok(())
}

fn validate_remote(remote: &str) -> io::Result<()> {
    validate_argument("Git remote", remote)?;
    if remote.chars().any(char::is_whitespace) {
        return Err(invalid_data("Git remote cannot contain whitespace"));
    }
    if remote
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split('/').next())
        .is_some_and(|authority| authority.contains('@'))
    {
        return Err(invalid_data(
            "Git remote contains embedded credentials; use SSH or a credential helper",
        ));
    }
    Ok(())
}

fn valid_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.chars().all(|c| c.is_ascii_hexdigit())
}

fn path_component(value: &str) -> String {
    let component: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if component.is_empty() {
        "unnamed".into()
    } else if matches!(component.as_str(), "." | "..") {
        component.replace('.', "_")
    } else {
        component
    }
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn portable_mcp(input: &Value) -> io::Result<(Value, Vec<String>)> {
    let mut portable = input.clone();
    let mut required_env = BTreeSet::new();
    {
        let object = portable
            .as_object_mut()
            .ok_or_else(|| invalid_data("MCP config must be an object"))?;
        reject_unknown_keys(
            object,
            &["$schema", "mcpServers", "disabledServers", "enabledServers"],
            "MCP config",
        )?;
        if let Some(schema) = object.get("$schema") {
            let schema = schema
                .as_str()
                .ok_or_else(|| invalid_data("MCP $schema must be a string"))?;
            if url_has_literal_secret(schema) || looks_like_secret_literal(schema) {
                return Err(invalid_data("MCP $schema contains a credential"));
            }
        }
        let disabled = mcp_name_list(object, "disabledServers")?;
        let enabled = mcp_name_list(object, "enabledServers")?;
        if !disabled.is_disjoint(&enabled) {
            return Err(invalid_data(
                "MCP server cannot be both enabled and disabled",
            ));
        }
        let servers = object
            .get_mut("mcpServers")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| invalid_data("MCP config must contain an mcpServers object"))?;

        for (server_name, server) in servers {
            let Some(server) = server.as_object_mut() else {
                return Err(invalid_data(format!(
                    "MCP server {server_name:?} must be an object"
                )));
            };
            reject_unknown_keys(
                server,
                &[
                    "enabled",
                    "timeout",
                    "requestIdFormat",
                    "type",
                    "command",
                    "args",
                    "env",
                    "cwd",
                    "url",
                    "headers",
                    "auth",
                    "oauth",
                ],
                &format!("MCP server {server_name:?}"),
            )?;
            validate_mcp_server_shape(server_name, server)?;

            if let Some(env) = server.get_mut("env") {
                sanitize_env(server_name, env, &mut required_env)?;
            }
            if let Some(headers) = server.get_mut("headers") {
                sanitize_headers(server_name, headers, &mut required_env)?;
            }
            collect_arg_environment(server_name, server.get("args"), &mut required_env)?;
            for field in ["auth", "oauth"] {
                if let Some(value) = server.get_mut(field) {
                    sanitize_secret_fields(
                        server_name,
                        &[field.to_string()],
                        value,
                        &mut required_env,
                    )?;
                }
            }
            reject_unhandled_secrets(server_name, server)?;
            collect_environment_references(&Value::Object(server.clone()), &mut required_env)?;
        }
    }

    if let Some(home) = crate::state::home_dir()
        .and_then(|path| path.to_str().map(str::to_string))
        .filter(|path| path.starts_with('/') && path != "/")
    {
        rewrite_home_paths(&mut portable, &home);
    }
    required_env.remove("HOME");
    Ok((portable, required_env.into_iter().collect()))
}

fn validate_mcp_server_shape(name: &str, server: &Map<String, Value>) -> io::Result<()> {
    if !valid_mcp_server_name(name) {
        return Err(invalid_data(format!("invalid MCP server name {name:?}")));
    }
    if server
        .get("enabled")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid_data(format!(
            "MCP server {name:?} enabled must be boolean"
        )));
    }
    if server
        .get("timeout")
        .is_some_and(|value| value.as_u64().is_none())
    {
        return Err(invalid_data(format!(
            "MCP server {name:?} timeout must be a non-negative integer"
        )));
    }
    if server
        .get("requestIdFormat")
        .is_some_and(|value| !matches!(value.as_str(), Some("number" | "string")))
    {
        return Err(invalid_data(format!(
            "MCP server {name:?} has invalid requestIdFormat"
        )));
    }
    for field in ["command", "cwd", "url"] {
        if server.get(field).is_some_and(|value| !value.is_string()) {
            return Err(invalid_data(format!(
                "MCP server {name:?} {field} must be a string"
            )));
        }
    }

    let transport = server.get("type").map_or(Ok("stdio"), |value| {
        value
            .as_str()
            .filter(|transport| matches!(*transport, "stdio" | "http" | "sse"))
            .ok_or_else(|| invalid_data(format!("MCP server {name:?} has invalid type")))
    })?;
    let required = if transport == "stdio" {
        "command"
    } else {
        "url"
    };
    if !server
        .get(required)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        return Err(invalid_data(format!(
            "MCP server {name:?} {transport} transport requires {required}"
        )));
    }
    Ok(())
}

fn valid_mcp_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

fn mcp_name_list(object: &Map<String, Value>, key: &str) -> io::Result<BTreeSet<String>> {
    let Some(value) = object.get(key) else {
        return Ok(BTreeSet::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| invalid_data(format!("{key} must be an array")))?;
    items
        .iter()
        .map(|item| {
            let name = item
                .as_str()
                .ok_or_else(|| invalid_data(format!("{key} must contain strings")))?;
            if !valid_mcp_server_name(name) {
                return Err(invalid_data(format!("{key} contains invalid server name")));
            }
            Ok(name.to_string())
        })
        .collect()
}

fn reject_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    context: &str,
) -> io::Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_data(format!(
            "{context} contains unsupported field {key:?}"
        )));
    }
    Ok(())
}

fn collect_environment_references(
    value: &Value,
    required_env: &mut BTreeSet<String>,
) -> io::Result<()> {
    match value {
        Value::Object(object) => {
            for child in object.values() {
                collect_environment_references(child, required_env)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_environment_references(child, required_env)?;
            }
        }
        Value::String(text) => {
            if let Some(references) = indirection_references(text)? {
                required_env.extend(references);
            }
        }
        _ => {}
    }
    Ok(())
}

fn rewrite_home_paths(value: &mut Value, home: &str) {
    match value {
        Value::Object(object) => {
            for child in object.values_mut() {
                rewrite_home_paths(child, home);
            }
        }
        Value::Array(items) => {
            for child in items {
                rewrite_home_paths(child, home);
            }
        }
        Value::String(text) if text.contains(home) => {
            *text = replace_home_path(text, home);
        }
        _ => {}
    }
}

fn replace_home_path(text: &str, home: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(home) {
        let before = rest[..index].chars().next_back();
        let after = rest[index + home.len()..].chars().next();
        let boundary_before = before.map_or(true, |c| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '=' | ':' | '(' | '[')
        });
        let boundary_after = after.map_or(true, |c| c == '/');
        output.push_str(&rest[..index]);
        if boundary_before && boundary_after {
            output.push_str("${HOME}");
            rest = &rest[index + home.len()..];
        } else {
            let first = rest[index..].chars().next().unwrap();
            output.push(first);
            rest = &rest[index + first.len_utf8()..];
        }
    }
    output.push_str(rest);
    output
}

fn collect_arg_environment(
    server_name: &str,
    value: Option<&Value>,
    required_env: &mut BTreeSet<String>,
) -> io::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let args = value
        .as_array()
        .ok_or_else(|| invalid_data(format!("MCP server {server_name:?} args must be an array")))?;
    let mut strings = Vec::with_capacity(args.len());
    for argument in args {
        strings.push(argument.as_str().ok_or_else(|| {
            invalid_data(format!(
                "MCP server {server_name:?} args must contain strings"
            ))
        })?);
    }
    for pair in strings.windows(2) {
        if matches!(pair[0], "-e" | "--env") && valid_env_name(pair[1]) {
            required_env.insert(pair[1].to_string());
        }
    }
    Ok(())
}

fn sanitize_env(
    server_name: &str,
    value: &mut Value,
    required_env: &mut BTreeSet<String>,
) -> io::Result<()> {
    let env = value
        .as_object_mut()
        .ok_or_else(|| invalid_data(format!("MCP server {server_name:?} env must be an object")))?;

    for (name, value) in env {
        if !valid_env_name(name) {
            return Err(invalid_data(format!(
                "MCP server {server_name:?} has invalid env name {name:?}"
            )));
        }
        let current = value.as_str().ok_or_else(|| {
            invalid_data(format!(
                "MCP server {server_name:?} env value {name:?} must be a string"
            ))
        })?;
        if let Some(references) = secret_indirection_references(current, false)? {
            required_env.extend(references);
            continue;
        }
        if current == name && valid_env_name(name) {
            required_env.insert(name.clone());
            continue;
        }

        let variable = if valid_env_name(name) {
            name.clone()
        } else {
            generated_variable(server_name, &["env", name])
        };
        *value = Value::String(variable.clone());
        required_env.insert(variable);
    }
    Ok(())
}

fn sanitize_headers(
    server_name: &str,
    value: &mut Value,
    required_env: &mut BTreeSet<String>,
) -> io::Result<()> {
    let headers = value.as_object_mut().ok_or_else(|| {
        invalid_data(format!(
            "MCP server {server_name:?} headers must be an object"
        ))
    })?;

    for (name, value) in headers {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(invalid_data(format!(
                "MCP server {server_name:?} has invalid header name {name:?}"
            )));
        }
        let current = value.as_str().ok_or_else(|| {
            invalid_data(format!(
                "MCP server {server_name:?} header {name:?} must be a string"
            ))
        })?;
        let authorization = name.eq_ignore_ascii_case("Authorization");
        if let Some(references) = secret_indirection_references(current, authorization)? {
            required_env.extend(references);
            continue;
        }

        let variable = generated_variable(server_name, &["headers", name]);
        let prefix = authorization
            .then(|| auth_scheme_prefix(current))
            .flatten()
            .unwrap_or("");
        *value = Value::String(format!("{prefix}${{{variable}}}"));
        required_env.insert(variable);
    }
    Ok(())
}

fn auth_scheme_prefix(value: &str) -> Option<&str> {
    ["Bearer ", "Basic ", "Token "]
        .into_iter()
        .find(|prefix| value.starts_with(prefix))
}

fn sanitize_secret_fields(
    server_name: &str,
    path: &[String],
    value: &mut Value,
    required_env: &mut BTreeSet<String>,
) -> io::Result<()> {
    let field = path.first().map(String::as_str).unwrap_or_default();
    let object = value.as_object_mut().ok_or_else(|| {
        invalid_data(format!(
            "MCP server {server_name:?} {field} must be an object"
        ))
    })?;
    let allowed = match field {
        "auth" => [
            "type",
            "credentialId",
            "tokenUrl",
            "clientId",
            "clientSecret",
            "resource",
        ]
        .as_slice(),
        "oauth" => [
            "clientId",
            "clientSecret",
            "redirectUri",
            "callbackPort",
            "callbackPath",
            "prompt",
        ]
        .as_slice(),
        _ => return Err(invalid_data("unknown MCP secret container")),
    };
    reject_unknown_keys(
        object,
        allowed,
        &format!("MCP server {server_name:?} {field}"),
    )?;
    object.remove("credentialId");

    for (name, child) in object {
        if sensitive_key(name) {
            let current = child.as_str().ok_or_else(|| {
                invalid_data(format!(
                    "MCP server {server_name:?} secret field {field}.{name} must be a string"
                ))
            })?;
            if let Some(references) = secret_indirection_references(current, false)? {
                required_env.extend(references);
            } else {
                let variable = generated_variable(server_name, &[field, name]);
                *child = Value::String(format!("${{{variable}}}"));
                required_env.insert(variable);
            }
        } else if let Some(text) = child.as_str() {
            if url_has_literal_secret(text) || looks_like_secret_literal(text) {
                return Err(invalid_data(format!(
                    "MCP server {server_name:?} contains a credential in {field}.{name}"
                )));
            }
        }
    }
    Ok(())
}

fn indirection_references(value: &str) -> io::Result<Option<Vec<String>>> {
    let mut references = Vec::new();
    let mut rest = value;
    let mut found = false;
    while let Some(start) = rest.find("${") {
        let after_start = &rest[start + 2..];
        let end = after_start
            .find('}')
            .ok_or_else(|| invalid_data("unterminated MCP environment placeholder"))?;
        let expression = &after_start[..end];
        let (name, default) = expression
            .split_once(":-")
            .map_or((expression, None), |(name, default)| (name, Some(default)));
        if !valid_env_name(name) {
            return Err(invalid_data(format!(
                "invalid MCP environment placeholder {expression:?}"
            )));
        }
        if let Some(default) = default {
            if looks_like_secret_literal(default) {
                return Err(invalid_data(format!(
                    "MCP environment placeholder {name:?} contains a secret-like default"
                )));
            }
        } else {
            references.push(name.to_string());
        }
        found = true;
        rest = &after_start[end + 1..];
    }
    Ok(found.then_some(references))
}

fn secret_indirection_references(
    value: &str,
    allow_auth_scheme: bool,
) -> io::Result<Option<Vec<String>>> {
    if value.starts_with('!') {
        if safe_secret_command(value) {
            return Ok(Some(Vec::new()));
        }
        return Err(invalid_data(
            "MCP secret command is not a recognized credential resolver",
        ));
    }
    let Some(references) = indirection_references(value)? else {
        return Ok(None);
    };

    let mut literal = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        literal.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let end = after_start
            .find('}')
            .ok_or_else(|| invalid_data("unterminated MCP environment placeholder"))?;
        let expression = &after_start[..end];
        if expression
            .split_once(":-")
            .is_some_and(|(_, default)| !default.is_empty())
        {
            return Err(invalid_data(
                "secret-bearing MCP placeholders cannot contain literal defaults",
            ));
        }
        rest = &after_start[end + 1..];
    }
    literal.push_str(rest);

    let allowed_literal = literal.is_empty()
        || (allow_auth_scheme
            && ["Bearer ", "Basic ", "Token "]
                .iter()
                .any(|scheme| literal.eq_ignore_ascii_case(scheme)));
    if !allowed_literal {
        return Err(invalid_data(
            "secret-bearing MCP values cannot mix literals with placeholders",
        ));
    }
    Ok(Some(references))
}

fn safe_secret_command(value: &str) -> bool {
    if looks_like_secret_literal(value) {
        return false;
    }
    let command = value.trim_start_matches('!').trim_start();
    if command.contains([';', '|', '&', '>', '<', '`', '\n', '\r']) || command.contains("$(") {
        return false;
    }
    command == "gh auth token"
        || [
            "printenv ",
            "gh auth token ",
            "cat ",
            "security find-generic-password ",
            "pass show ",
            "op read ",
            "aws secretsmanager ",
        ]
        .iter()
        .any(|prefix| command.starts_with(prefix))
}

fn reject_unhandled_secrets(
    server_name: &str,
    server: &serde_json::Map<String, Value>,
) -> io::Result<()> {
    if let Some(args) = server.get("args").and_then(Value::as_array) {
        let strings: Vec<&str> = args
            .iter()
            .map(|argument| {
                argument.as_str().ok_or_else(|| {
                    invalid_data(format!(
                        "MCP server {server_name:?} args must contain strings"
                    ))
                })
            })
            .collect::<io::Result<_>>()?;
        for argument in &strings {
            if let Some((flag, candidate)) = argument.split_once('=') {
                match flag {
                    "-H" | "--header" => validate_mcp_header_argument(server_name, candidate)?,
                    "-e" | "--env" => validate_mcp_env_argument(server_name, candidate)?,
                    _ if (matches!(flag, "-u" | "--user" | "--proxy-user")
                        || (flag.starts_with('-') && sensitive_key(flag)))
                        && secret_indirection_references(candidate, false)?.is_none() =>
                    {
                        return Err(invalid_data(format!(
                            "MCP server {server_name:?} has a literal secret in argument {flag:?}"
                        )));
                    }
                    _ => {}
                }
            }
        }
        for pair in strings.windows(2) {
            let flag = pair[0];
            let candidate = pair[1];
            match flag {
                "-H" | "--header" => validate_mcp_header_argument(server_name, candidate)?,
                "-e" | "--env" => validate_mcp_env_argument(server_name, candidate)?,
                _ if (matches!(flag, "-u" | "--user" | "--proxy-user")
                    || (flag.starts_with('-') && sensitive_key(flag)))
                    && secret_indirection_references(candidate, false)?.is_none() =>
                {
                    return Err(invalid_data(format!(
                        "MCP server {server_name:?} has a literal secret after argument {flag:?}"
                    )));
                }
                _ => {}
            }
        }
    }
    reject_secret_value(server_name, &mut Vec::new(), &Value::Object(server.clone()))
}

fn validate_mcp_header_argument(server_name: &str, candidate: &str) -> io::Result<()> {
    let (name, value) = candidate.split_once(':').ok_or_else(|| {
        invalid_data(format!(
            "MCP server {server_name:?} has a malformed header argument"
        ))
    })?;
    if candidate.contains(['\r', '\n']) {
        return Err(invalid_data(format!(
            "MCP server {server_name:?} header argument contains a line break"
        )));
    }
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "accept" | "content-type" | "user-agent" | "mcp-protocol-version"
    ) {
        return Ok(());
    }
    let authorization = name.eq_ignore_ascii_case("Authorization");
    if value.trim_start().starts_with('!')
        || secret_indirection_references(value.trim_start(), authorization)?.is_none()
    {
        return Err(invalid_data(format!(
            "MCP server {server_name:?} has a literal header argument"
        )));
    }
    Ok(())
}

fn validate_mcp_env_argument(server_name: &str, candidate: &str) -> io::Result<()> {
    if valid_env_name(candidate) {
        return Ok(());
    }
    let (name, value) = candidate.split_once('=').ok_or_else(|| {
        invalid_data(format!(
            "MCP server {server_name:?} has a malformed environment argument"
        ))
    })?;
    if !valid_env_name(name) {
        return Err(invalid_data(format!(
            "MCP server {server_name:?} has an invalid environment name"
        )));
    }
    if value.starts_with('!') || secret_indirection_references(value, false)?.is_none() {
        return Err(invalid_data(format!(
            "MCP server {server_name:?} environment assignments must use a placeholder"
        )));
    }
    Ok(())
}

fn reject_secret_value(server_name: &str, path: &mut Vec<String>, value: &Value) -> io::Result<()> {
    match value {
        Value::Object(object) => {
            for (name, child) in object {
                path.push(name.clone());
                if sensitive_key(name) {
                    let text = child.as_str().ok_or_else(|| {
                        invalid_data(format!(
                            "MCP server {server_name:?} secret field {} must be a string",
                            path.join(".")
                        ))
                    })?;
                    let parent = path
                        .get(path.len().saturating_sub(2))
                        .map(String::as_str)
                        .unwrap_or_default();
                    let env_reference = parent == "env" && text == name && valid_env_name(text);
                    let authorization =
                        parent == "headers" && name.eq_ignore_ascii_case("Authorization");
                    if !env_reference
                        && secret_indirection_references(text, authorization)?.is_none()
                    {
                        return Err(invalid_data(format!(
                            "MCP server {server_name:?} still contains a literal secret in {}",
                            path.join(".")
                        )));
                    }
                } else {
                    reject_secret_value(server_name, path, child)?;
                }
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(index.to_string());
                reject_secret_value(server_name, path, child)?;
                path.pop();
            }
        }
        Value::String(text) => {
            if url_has_literal_secret(text) {
                return Err(invalid_data(format!(
                    "MCP server {server_name:?} URL contains a literal credential"
                )));
            }
            if looks_like_secret_literal(text)
                && secret_indirection_references(text, false)?.is_none()
            {
                return Err(invalid_data(format!(
                    "MCP server {server_name:?} contains a secret-like literal in {}",
                    path.join(".")
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

fn url_has_literal_secret(value: &str) -> bool {
    let Some((_, remainder)) = value.split_once("://") else {
        return false;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return true;
    }
    let Some((_, query_and_fragment)) = remainder.split_once('?') else {
        return false;
    };
    let query = query_and_fragment.split('#').next().unwrap_or_default();
    query.split('&').any(|pair| {
        let Some((_, value)) = pair.split_once('=') else {
            return false;
        };
        !value.is_empty()
            && secret_indirection_references(value, false)
                .ok()
                .flatten()
                .is_none()
    })
}

fn looks_like_secret_literal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("github_pat_")
        || lower.contains("ghp_")
        || lower.contains("gho_")
        || lower.contains("ghu_")
        || lower.contains("ghs_")
        || lower.contains("ghr_")
        || lower.contains("xoxb-")
        || lower.contains("xoxp-")
        || lower.contains("xoxa-")
        || lower.contains("xoxr-")
        || lower.contains("glpat-")
        || lower.contains("npm_")
        || value.contains("AIza")
        || value.contains("AKIA")
        || value.contains("ASIA")
        || lower.contains("-----begin private key-----")
        || lower.starts_with("sk-")
}

fn sensitive_key(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "authorization"
            | "cookie"
            | "password"
            | "secret"
            | "clientsecret"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "apikey"
            | "privatekey"
            | "credential"
            | "awsaccesskeyid"
            | "awssecretaccesskey"
    )
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn generated_variable(server_name: &str, path: &[&str]) -> String {
    let mut variable = format!("SYNAPSE_MCP_{}", env_component(server_name));
    for part in path {
        variable.push('_');
        variable.push_str(&env_component(part));
    }
    variable
}

fn env_component(name: &str) -> String {
    let mut out = String::new();
    let mut previous_lowercase = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && previous_lowercase && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_uppercase());
            previous_lowercase = c.is_ascii_lowercase();
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
            previous_lowercase = false;
        }
    }
    out.trim_matches('_').to_string()
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct EnvGuard {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &Path) -> Self {
            let previous = env::var_os(name);
            env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(self.name, value),
                None => env::remove_var(self.name),
            }
        }
    }

    fn tmpdir(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "synapse-stack-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn portable_mcp_removes_local_credentials() {
        let input = json!({
            "$schema": "https://example.test/mcp-schema.json",
            "mcpServers": {
                "github": {
                    "type": "http",
                    "url": "https://api.githubcopilot.com/mcp/",
                    "headers": {
                        "Authorization": "Bearer github_pat_secret",
                        "X-Mode": "stable"
                    },
                    "auth": {
                        "type": "oauth",
                        "credentialId": "local-row",
                        "clientSecret": "oauth-secret"
                    }
                },
                "jira": {
                    "command": "npx",
                    "args": ["-y", "@example/jira-mcp"],
                    "env": {
                        "JIRA_URL": "https://jira.example.test",
                        "JIRA_API_TOKEN": "token-value"
                    }
                }
            }
        });

        let (portable, required_env) = portable_mcp(&input).unwrap();
        let encoded = serde_json::to_string(&portable).unwrap();

        assert!(!encoded.contains("github_pat_secret"));
        assert!(!encoded.contains("oauth-secret"));
        assert!(!encoded.contains("local-row"));
        assert!(!encoded.contains("token-value"));
        assert_eq!(
            portable["mcpServers"]["jira"]["env"]["JIRA_URL"],
            "JIRA_URL"
        );
        assert_eq!(
            portable["mcpServers"]["github"]["headers"]["Authorization"],
            "Bearer ${SYNAPSE_MCP_GITHUB_HEADERS_AUTHORIZATION}"
        );
        assert_eq!(
            portable["mcpServers"]["github"]["headers"]["X-Mode"],
            "${SYNAPSE_MCP_GITHUB_HEADERS_X_MODE}"
        );
        assert!(portable["mcpServers"]["github"]["auth"]
            .get("credentialId")
            .is_none());
        assert_eq!(
            required_env,
            vec![
                "JIRA_API_TOKEN",
                "JIRA_URL",
                "SYNAPSE_MCP_GITHUB_AUTH_CLIENT_SECRET",
                "SYNAPSE_MCP_GITHUB_HEADERS_AUTHORIZATION",
                "SYNAPSE_MCP_GITHUB_HEADERS_X_MODE",
            ]
        );
    }

    #[test]
    fn portable_mcp_preserves_existing_secret_indirection() {
        let input = json!({
            "mcpServers": {
                "github": {
                    "type": "http",
                    "url": "https://${MCP_HOST}/mcp",
                    "headers": {
                        "Authorization": "Bearer ${GITHUB_TOKEN}",
                        "X-API-Key": "!security find-generic-password -w -s mcp-key"
                    }
                },
                "local": {
                    "command": "node",
                    "args": ["-e", "DOCKER_TOKEN"],
                    "env": {
                        "TOKEN": "${LOCAL_TOKEN}",
                        "PASSWORD": "!pass show local/password"
                    }
                }
            }
        });

        let (portable, required_env) = portable_mcp(&input).unwrap();

        assert_eq!(
            portable["mcpServers"]["github"]["headers"]["Authorization"],
            "Bearer ${GITHUB_TOKEN}"
        );
        assert_eq!(
            portable["mcpServers"]["github"]["headers"]["X-API-Key"],
            "!security find-generic-password -w -s mcp-key"
        );
        assert_eq!(
            portable["mcpServers"]["local"]["env"]["TOKEN"],
            "${LOCAL_TOKEN}"
        );
        assert_eq!(
            portable["mcpServers"]["local"]["env"]["PASSWORD"],
            "!pass show local/password"
        );
        assert_eq!(
            required_env,
            vec!["DOCKER_TOKEN", "GITHUB_TOKEN", "LOCAL_TOKEN", "MCP_HOST"]
        );
    }

    #[test]
    fn portable_mcp_rewrites_home_paths() {
        let home = crate::state::home_dir().unwrap();
        let input = json!({
            "mcpServers": {
                "local": {
                    "command": "node",
                    "args": [home.join("server.js").to_string_lossy()],
                    "env": {
                        "TOKEN": format!("!cat {}/.config/token", home.display())
                    }
                }
            }
        });

        let (portable, _) = portable_mcp(&input).unwrap();
        assert_eq!(
            portable["mcpServers"]["local"]["args"][0],
            "${HOME}/server.js"
        );
        assert_eq!(
            portable["mcpServers"]["local"]["env"]["TOKEN"],
            "!cat ${HOME}/.config/token"
        );
    }

    #[test]
    fn portable_mcp_qualifies_generated_names_by_field_path() {
        let input = json!({
            "mcpServers": {
                "service": {
                    "type": "http",
                    "url": "https://example.test/mcp",
                    "headers": {
                        "Cookie": "session=ordinary-secret; theme=dark",
                        "X-API-Key": "header-secret"
                    },
                    "auth": {
                        "clientSecret": "auth-secret",
                        "tokenUrl": "https://example.test/oauth/token"
                    },
                    "oauth": {"clientSecret": "oauth-secret"}
                }
            }
        });

        let (portable, required_env) = portable_mcp(&input).unwrap();

        assert_eq!(
            portable["mcpServers"]["service"]["headers"]["X-API-Key"],
            "${SYNAPSE_MCP_SERVICE_HEADERS_X_API_KEY}"
        );
        assert_eq!(
            portable["mcpServers"]["service"]["headers"]["Cookie"],
            "${SYNAPSE_MCP_SERVICE_HEADERS_COOKIE}"
        );
        assert_eq!(
            portable["mcpServers"]["service"]["auth"]["clientSecret"],
            "${SYNAPSE_MCP_SERVICE_AUTH_CLIENT_SECRET}"
        );
        assert_eq!(
            portable["mcpServers"]["service"]["auth"]["tokenUrl"],
            "https://example.test/oauth/token"
        );
        assert_eq!(
            portable["mcpServers"]["service"]["oauth"]["clientSecret"],
            "${SYNAPSE_MCP_SERVICE_OAUTH_CLIENT_SECRET}"
        );
        assert_eq!(
            required_env,
            vec![
                "SYNAPSE_MCP_SERVICE_AUTH_CLIENT_SECRET",
                "SYNAPSE_MCP_SERVICE_HEADERS_COOKIE",
                "SYNAPSE_MCP_SERVICE_HEADERS_X_API_KEY",
                "SYNAPSE_MCP_SERVICE_OAUTH_CLIENT_SECRET",
            ]
        );
    }

    #[test]
    fn portable_mcp_rejects_unhandled_inline_secrets() {
        for input in [
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp?access_token=secret"
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "local": {
                        "command": "node",
                        "args": ["--token", "ordinary-secret-value"]
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "headers": {
                            "Authorization": "Bearer ${TOKEN:-ordinary-secret}"
                        }
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "headers": {
                            "Authorization": "prefix=${TOKEN}"
                        }
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "local": {
                        "command": "node",
                        "env": {
                            "TOKEN": "${TOKEN:-ordinary-secret}"
                        }
                    }
                }
            }),
            json!({
                "token": "ordinary-secret",
                "mcpServers": {}
            }),
            json!({
                "mcpServers": {
                    "local": {
                        "command": "node",
                        "args": ["--api-key=ordinary-secret"]
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "auth": "ordinary-secret"
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "auth": {"password": ["ordinary-secret"]}
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "headers": {
                            "Authorization": "!echo ordinary-secret"
                        }
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "headers": {
                            "Authorization": "!printenv TOKEN; echo ordinary-secret"
                        }
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "local": {
                        "command": "docker",
                        "args": ["--env=API_TOKEN=ordinary-secret"]
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "local": {
                        "command": "node",
                        "args": ["--header=Authorization: Bearer ordinary-secret"]
                    }
                }
            }),
            json!({
                "mcpServers": {
                    "local": {
                        "command": {},
                        "enabled": "yes"
                    }
                }
            }),
        ] {
            assert!(portable_mcp(&input).is_err());
        }
    }

    #[test]
    fn capture_pins_plugins_and_externalizes_mcp_environment() {
        let _lock = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tmpdir("capture");
        let omp = temp.join("omp");
        let output = temp.join("stack");
        let missing_skillshare = temp.join("missing-skillshare.yaml");
        let _omp = EnvGuard::set("SYNAPSE_OMP_DIR", &omp);
        let _skillshare = EnvGuard::set("SYNAPSE_SKILLSHARE_CONFIG", &missing_skillshare);

        fs::create_dir_all(omp.join("plugins")).unwrap();
        fs::create_dir_all(omp.join("agent")).unwrap();
        fs::write(
            omp.join("plugins/package.json"),
            serde_json::to_vec(&json!({
                "dependencies": {"@example/plugin": "^1.0.0"}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            omp.join("plugins/omp-plugins.lock.json"),
            serde_json::to_vec(&json!({
                "plugins": {
                    "@example/plugin": {
                        "version": "1.2.3",
                        "enabled": false,
                        "enabledFeatures": ["fast"]
                    }
                },
                "settings": {
                    "@example/plugin": {"mode": "full"}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            omp.join("agent/mcp.json"),
            serde_json::to_vec(&json!({
                "mcpServers": {
                    "service": {
                        "command": "npx",
                        "env": {"SERVICE_TOKEN": "literal-secret"}
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let manifest = capture_into(&output).unwrap();
        assert_eq!(
            manifest.omp_profiles[0].plugins[0],
            PortablePlugin {
                name: "@example/plugin".into(),
                source: PluginSource::Package {
                    spec: "@example/plugin@1.2.3".into()
                },
                enabled: false,
                enabled_features: Some(vec!["fast".into()]),
            }
        );
        assert_eq!(
            manifest.omp_profiles[0].mcp.as_ref().unwrap()["mcpServers"]["service"]["env"]
                ["SERVICE_TOKEN"],
            "SERVICE_TOKEN"
        );
        assert_eq!(manifest.omp_profiles[0].required_env, vec!["SERVICE_TOKEN"]);
        assert!(!fs::read_to_string(output.join(MANIFEST_FILE))
            .unwrap()
            .contains("literal-secret"));

        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn capture_omits_plugin_settings_from_manifest() {
        let _lock = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tmpdir("plugin-secret");
        let omp = temp.join("omp");
        let output = temp.join("stack");
        let missing_skillshare = temp.join("missing-skillshare.yaml");
        let _omp = EnvGuard::set("SYNAPSE_OMP_DIR", &omp);
        let _skillshare = EnvGuard::set("SYNAPSE_SKILLSHARE_CONFIG", &missing_skillshare);
        fs::create_dir_all(omp.join("plugins")).unwrap();
        fs::write(
            omp.join("plugins/package.json"),
            r#"{"dependencies":{"example-plugin":"1.2.3"}}"#,
        )
        .unwrap();
        fs::write(
            omp.join("plugins/omp-plugins.lock.json"),
            r#"{
              "plugins":{"example-plugin":{"version":"1.2.3","enabled":true}},
              "settings":{"example-plugin":{"github_token_value":"ordinary-secret"}}
            }"#,
        )
        .unwrap();

        let manifest = capture_into(&output).unwrap();
        let encoded = fs::read_to_string(output.join(MANIFEST_FILE)).unwrap();
        assert_eq!(manifest.omp_profiles[0].plugins.len(), 1);
        assert!(!encoded.contains("github_token_value"));
        assert!(!encoded.contains("ordinary-secret"));
        assert!(!encoded.contains("\"settings\""));

        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn package_sources_are_exact_and_keep_alias_identity() {
        assert!(!exact_registry_version("latest"));
        assert!(!exact_registry_version("^1.2.3"));
        assert!(!exact_registry_version("1"));
        assert!(!exact_registry_version("1.2"));
        assert!(!exact_registry_version("1latest"));
        assert!(exact_registry_version("1.2.3-beta.1"));
        assert_eq!(
            npm_restore_spec("alias", "npm:@scope/plugin@^1", "1.4.0").unwrap(),
            "alias@npm:@scope/plugin@1.4.0"
        );
        assert!(validate_package_source("safe", "safe@1.2.3").is_ok());
        assert!(
            validate_package_source("safe", "safe@https://attacker.example/plugin.tgz@1.2.3")
                .is_err()
        );
        assert!(validate_package_source("alias", "alias@npm:@scope/plugin@1.4.0").is_ok());
        assert_eq!(
            pinned_git_dependency("owner/repo#0123456789abcdef0123456789abcdef01234567"),
            Some((
                "github:owner/repo".into(),
                "0123456789abcdef0123456789abcdef01234567".into()
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_declared_git_origin_restores_without_installed_checkout() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tmpdir("git-origin");
        let staging = temp.join("stack");
        let log = temp.join("omp.log");
        let fake_omp = temp.join("omp-fake");
        let revision = "0123456789abcdef0123456789abcdef01234567";
        fs::create_dir_all(&staging).unwrap();
        fs::write(
            &fake_omp,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SYNAPSE_TEST_LOG\"\n",
        )
        .unwrap();
        fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755)).unwrap();
        let _omp_bin = EnvGuard::set("SYNAPSE_OMP_BIN", &fake_omp);
        let _log = EnvGuard::set("SYNAPSE_TEST_LOG", &log);

        let source = capture_plugin_source(
            DEFAULT_PROFILE,
            "example-plugin",
            Some(&format!("github:owner/repo#{revision}")),
            Some("1.0.0"),
            &temp.join("missing-plugins"),
            &staging,
        )
        .unwrap();
        assert_eq!(
            source,
            PluginSource::Git {
                url: "github:owner/repo".into(),
                rev: revision.into(),
            }
        );

        restore_plugin(
            &staging,
            DEFAULT_PROFILE,
            &PortablePlugin {
                name: "example-plugin".into(),
                source,
                enabled: true,
                enabled_features: None,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            format!("plugin install github:owner/repo#{revision}\n")
        );

        fs::remove_dir_all(temp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn capture_snapshots_unmanaged_local_plugins() {
        use std::os::unix::fs::symlink;

        let _lock = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tmpdir("snapshot");
        let omp = temp.join("omp");
        let source = temp.join("local-plugin");
        let output = temp.join("stack");
        let missing_skillshare = temp.join("missing-skillshare.yaml");
        let _omp = EnvGuard::set("SYNAPSE_OMP_DIR", &omp);
        let _skillshare = EnvGuard::set("SYNAPSE_SKILLSHARE_CONFIG", &missing_skillshare);

        fs::create_dir_all(omp.join("plugins/node_modules")).unwrap();
        fs::create_dir_all(source.join("src")).unwrap();
        fs::write(source.join("package.json"), r#"{"name":"local"}"#).unwrap();
        fs::write(source.join("src/index.js"), "export default {};\n").unwrap();
        fs::write(omp.join("plugins/package.json"), r#"{"dependencies":{}}"#).unwrap();
        fs::write(
            omp.join("plugins/omp-plugins.lock.json"),
            r#"{"plugins":{"local":{"version":"0.1.0","enabled":true,"enabledFeatures":null}},"settings":{}}"#,
        )
        .unwrap();
        symlink(&source, omp.join("plugins/node_modules/local")).unwrap();

        let manifest = capture_into(&output).unwrap();
        assert!(matches!(
            manifest.omp_profiles[0].plugins[0].source,
            PluginSource::Snapshot { .. }
        ));
        assert_eq!(
            fs::read_to_string(output.join("local-plugins/default/local/src/index.js")).unwrap(),
            "export default {};\n"
        );

        fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn mcp_restore_requires_force_for_conflicting_servers() {
        let existing = json!({
            "mcpServers": {
                "existing": {"command": "old"},
                "local": {"command": "keep"}
            }
        });
        let portable = json!({
            "mcpServers": {
                "existing": {"command": "new"},
                "remote": {"url": "https://example.test/mcp", "type": "http"}
            }
        });

        assert_eq!(
            merge_mcp(Some(&existing), &portable, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let merged = merge_mcp(Some(&existing), &portable, true).unwrap();
        assert_eq!(merged["mcpServers"]["existing"]["command"], "new");
        assert_eq!(merged["mcpServers"]["local"]["command"], "keep");
        assert_eq!(
            merged["mcpServers"]["remote"]["url"],
            "https://example.test/mcp"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_replays_profile_plugins_and_mcp() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tmpdir("restore");
        let omp = temp.join("omp");
        let config = temp.join("config");
        let input = temp.join("stack");
        let log = temp.join("omp.log");
        let fake_omp = temp.join("omp-fake");
        fs::create_dir_all(&input).unwrap();
        fs::write(
            &fake_omp,
            "#!/bin/sh\nprintf '%s|%s\\n' \"${OMP_PROFILE:-default}\" \"$*\" >> \"$SYNAPSE_TEST_LOG\"\n",
        )
        .unwrap();
        fs::set_permissions(&fake_omp, fs::Permissions::from_mode(0o755)).unwrap();
        let _omp_dir = EnvGuard::set("SYNAPSE_OMP_DIR", &omp);
        let _omp_bin = EnvGuard::set("SYNAPSE_OMP_BIN", &fake_omp);
        let _config = EnvGuard::set("XDG_CONFIG_HOME", &config);
        let _log = EnvGuard::set("SYNAPSE_TEST_LOG", &log);

        let manifest = StackManifest {
            version: STACK_VERSION,
            omp_profiles: vec![OmpProfile {
                name: "work".into(),
                plugins: vec![PortablePlugin {
                    name: "@example/plugin".into(),
                    source: PluginSource::Package {
                        spec: "@example/plugin@1.2.3".into(),
                    },
                    enabled: true,
                    enabled_features: None,
                }],
                mcp: Some(json!({
                    "mcpServers": {
                        "remote": {
                            "type": "http",
                            "url": "https://example.test/mcp"
                        }
                    }
                })),
                required_env: Vec::new(),
            }],
            skillshare: None,
        };

        validate_manifest(&manifest).unwrap();
        restore_from(&input, &manifest, false).unwrap();
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            "work|plugin install @example/plugin@1.2.3\n"
        );
        assert_eq!(
            read_json_optional(&omp.join("profiles/work/agent/mcp.json"))
                .unwrap()
                .unwrap()["mcpServers"]["remote"]["url"],
            "https://example.test/mcp"
        );
        assert_eq!(
            read_json_optional(&omp.join("profiles/work/plugins/omp-plugins.lock.json"))
                .unwrap()
                .unwrap()["plugins"]["@example/plugin"]["enabled"],
            true
        );

        fs::remove_dir_all(temp).ok();
    }
    #[test]
    fn restore_requires_explicit_code_trust() {
        let error = restore(
            Some(Path::new("/path/that/does/not/exist")),
            None,
            "root",
            false,
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn restore_remote_initializes_and_verifies_skillshare() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tmpdir("skillshare-restore");
        let config = temp.join("config");
        let input = temp.join("stack");
        let log = temp.join("skillshare.log");
        let fake_skillshare = temp.join("skillshare-fake");
        let fake_git = temp.join("git-fake");
        let remote = "git@github.com:example/ai-stack.git";
        fs::create_dir_all(&input).unwrap();
        fs::write(
            &fake_skillshare,
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$SYNAPSE_TEST_LOG\"\n\
             if [ \"$1\" = init ]; then\n\
               mkdir -p \"$XDG_CONFIG_HOME/skillshare\"\n\
               printf 'git_root: root\\n' > \"$XDG_CONFIG_HOME/skillshare/config.yaml\"\n\
             fi\n",
        )
        .unwrap();
        fs::write(
            &fake_git,
            "#!/bin/sh\n\
             if [ \"$3\" = remote ] && [ \"$4\" = get-url ]; then\n\
               printf '%s\\n' \"$SYNAPSE_TEST_REMOTE\"\n\
               exit 0\n\
             fi\n\
             exit 1\n",
        )
        .unwrap();
        fs::set_permissions(&fake_skillshare, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755)).unwrap();
        let _config = EnvGuard::set("XDG_CONFIG_HOME", &config);
        let _omp = EnvGuard::set("SYNAPSE_OMP_DIR", &temp.join("omp"));
        let _skillshare = EnvGuard::set("SYNAPSE_SKILLSHARE_BIN", &fake_skillshare);
        let _git = EnvGuard::set("SYNAPSE_GIT_BIN", &fake_git);
        let _log = EnvGuard::set("SYNAPSE_TEST_LOG", &log);
        let _remote = EnvGuard::set("SYNAPSE_TEST_REMOTE", Path::new(remote));
        let manifest = StackManifest {
            version: STACK_VERSION,
            omp_profiles: Vec::new(),
            skillshare: Some(SkillshareSource {
                remote: remote.into(),
                git_root: "root".into(),
            }),
        };
        write_json(&input.join(MANIFEST_FILE), &manifest).unwrap();

        restore(Some(&input), Some(remote), "root", false, false).unwrap();
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            format!("init --git-root root --remote {remote} --all-targets --no-copy --no-skill\n")
        );

        restore(Some(&input), None, "root", true, false).unwrap();
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            format!(
                "init --git-root root --remote {remote} --all-targets --no-copy --no-skill\n\
                 sync --all\n"
            )
        );

        fs::remove_dir_all(temp).ok();
    }
}
