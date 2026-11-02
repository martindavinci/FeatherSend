//! Build tasks: `cargo xtask <command>`.
//!
//! - `build-win7 [--arch x64|x86|all]`: release builds for the Windows 7
//!   targets (pinned nightly, build-std, static CRT, VS 2022 toolset).
//! - `check-imports <exe>...`: fails if an exe imports an API missing on Windows 7.
//! - `check-tag <tag>`: fails unless the tag is `v` + the workspace version.
//! - `dist`: both builds, import check, portable zips, installers, SHA256SUMS.
//! - `licenses`: renders packaging/THIRD-PARTY-LICENSES.txt, after a dependency
//!   change (needs `cargo install cargo-about --locked --features cli`).
//! - `vendor-localsend <commit>`: refreshes vendor/localsend-core.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const FORBIDDEN: [&str; 6] = [
    "GetHostNameW",
    "ProcessPrng",
    "WaitOnAddress",
    "WakeByAddressSingle",
    "WakeByAddressAll",
    "GetSystemTimePreciseAsFileTime",
];
const FORBIDDEN_DLL_PREFIXES: [&str; 4] = ["combase", "api-ms-win-", "vcruntime", "ucrtbase"];

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("build-win7") => build_win7_cmd(&args[1..]).map(|_| ()),
        Some("check-imports") => check_imports_cmd(&args[1..]),
        Some("check-tag") => check_tag(args.get(1).map(String::as_str).unwrap_or_default()),
        Some("dist") => dist(),
        Some("licenses") => licenses(),
        Some("vendor-localsend") => vendor_localsend(args.get(1).map(String::as_str)),
        _ => Err(
            "usage: cargo xtask build-win7|check-imports|check-tag|dist|licenses|vendor-localsend"
                .into(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn version() -> Result<String> {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).map_err(|e| e.to_string())?;
    workspace_version(&manifest).ok_or_else(|| "no [workspace.package] version".into())
}

fn workspace_version(manifest: &str) -> Option<String> {
    let section = manifest.split("[workspace.package]").nth(1)?;
    let line = section.lines().find(|l| l.trim_start().starts_with("version"))?;
    Some(line.split('"').nth(1)?.to_string())
}

fn check_tag(tag: &str) -> Result<()> {
    let expected = format!("v{}", version()?);
    if tag == expected { Ok(()) } else { Err(format!("tag {tag} does not match {expected}")) }
}

#[derive(Clone, Copy)]
enum Arch {
    X64,
    X86,
}

impl Arch {
    fn triple(self) -> &'static str {
        match self {
            Arch::X64 => "x86_64-win7-windows-msvc",
            Arch::X86 => "i686-win7-windows-msvc",
        }
    }
    fn vcvars(self) -> &'static str {
        match self {
            Arch::X64 => "x64",
            Arch::X86 => "amd64_x86",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Arch::X64 => "x64",
            Arch::X86 => "x86",
        }
    }
}

fn parse_arch(args: &[String]) -> Result<Vec<Arch>> {
    let value = args.iter().position(|a| a == "--arch").and_then(|i| args.get(i + 1));
    match value.map(String::as_str).unwrap_or("all") {
        "x64" => Ok(vec![Arch::X64]),
        "x86" => Ok(vec![Arch::X86]),
        "all" => Ok(vec![Arch::X64, Arch::X86]),
        other => Err(format!("unknown arch {other}")),
    }
}

/// Short target dir: build-std paths exceed MAX_PATH under long folders.
fn target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FS_TARGET_DIR") {
        return dir.into();
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| "C:\\".into()).join("fst")
}

/// The VS 2022 (v143) developer environment: VS 2026 libraries no longer
/// target Windows 7/8.1.
fn vs2022_env(arch: Arch) -> Result<Vec<(String, String)>> {
    let vswhere =
        PathBuf::from(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    let out = Command::new(&vswhere)
        .args(["-version", "[17.0,18.0)", "-products", "*", "-requires"])
        .arg("Microsoft.VisualStudio.Component.VC.Tools.x86.x64")
        .args(["-property", "installationPath", "-latest"])
        .output()
        .map_err(|e| format!("vswhere: {e}"))?;
    let install = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if install.is_empty() {
        return Err("Visual Studio 2022 with the C++ tools is required".into());
    }
    let vcvars = Path::new(&install).join(r"VC\Auxiliary\Build\vcvarsall.bat");
    // raw_arg: cmd.exe does not understand the backslash-escaped quotes
    // that `arg` would produce.
    use std::os::windows::process::CommandExt;
    let out = Command::new("cmd")
        .raw_arg(format!("/c \"call \"{}\" {} >nul && set\"", vcvars.display(), arch.vcvars()))
        .output()
        .map_err(|e| format!("vcvarsall: {e}"))?;
    if !out.status.success() {
        return Err("vcvarsall failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{cmd:?} failed with {status}")) }
}

fn build_win7(arch: Arch) -> Result<PathBuf> {
    let env = vs2022_env(arch)?;
    let target = target_dir();
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root())
        .envs(env)
        .env("CARGO_TARGET_DIR", &target)
        .env("RUSTFLAGS", "-C target-feature=+crt-static")
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "feathersend",
            "-Z",
            "build-std=std,panic_abort",
        ])
        .args(["--target", arch.triple()]);
    run(&mut cmd)?;
    let exe = target.join(arch.triple()).join("release").join("FeatherSend.exe");
    println!("built {}", exe.display());
    Ok(exe)
}

fn build_win7_cmd(args: &[String]) -> Result<Vec<PathBuf>> {
    parse_arch(args)?.into_iter().map(build_win7).collect()
}

fn parse_imports(dumpbin: &str) -> Vec<(String, String)> {
    let mut dll = String::new();
    let mut out = Vec::new();
    for line in dumpbin.lines() {
        let t = line.trim();
        if t.to_ascii_lowercase().ends_with(".dll") && !t.contains(' ') {
            dll = t.to_ascii_lowercase();
        } else if !dll.is_empty() {
            let mut parts = t.split_whitespace();
            if let (Some(hint), Some(name), None) = (parts.next(), parts.next(), parts.next())
                && u32::from_str_radix(hint, 16).is_ok()
            {
                out.push((dll.clone(), name.to_string()));
            }
        }
    }
    out
}

fn violations(imports: &[(String, String)]) -> Vec<String> {
    let mut bad: Vec<String> = imports
        .iter()
        .filter(|(dll, name)| {
            FORBIDDEN.contains(&name.as_str())
                || FORBIDDEN_DLL_PREFIXES.iter().any(|p| dll.starts_with(p))
        })
        .map(|(dll, name)| format!("{dll}!{name}"))
        .collect();
    bad.sort();
    bad.dedup();
    bad
}

fn check_imports(exe: &Path) -> Result<()> {
    let env = vs2022_env(Arch::X64)?;
    let out = Command::new("dumpbin")
        .envs(env)
        .arg("/imports")
        .arg(exe)
        .output()
        .map_err(|e| format!("dumpbin: {e}"))?;
    let imports = parse_imports(&String::from_utf8_lossy(&out.stdout));
    if imports.is_empty() {
        return Err(format!("no imports read from {}", exe.display()));
    }
    let mut dlls: Vec<&str> = imports.iter().map(|(d, _)| d.as_str()).collect();
    dlls.sort();
    dlls.dedup();
    println!("{}: {} imports from {}", exe.display(), imports.len(), dlls.join(", "));
    let bad = violations(&imports);
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!("APIs missing on Windows 7 imported by {}: {}", exe.display(), bad.join(", ")))
    }
}

fn check_imports_cmd(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("usage: cargo xtask check-imports <exe>...".into());
    }
    args.iter().try_for_each(|a| check_imports(Path::new(a)))
}

fn powershell(script: &str) -> Result<()> {
    run(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]))
}

fn iscc() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe"),
        PathBuf::from(r"C:\Program Files\Inno Setup 6\ISCC.exe"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// A release without installers must fail, not pass with only the zips.
fn require_iscc(found: Option<PathBuf>) -> Result<PathBuf> {
    found.ok_or_else(|| "Inno Setup 6 is required to build the installers".into())
}

/// `sha256sum -c` treats a CR as part of the file name.
fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn dist() -> Result<()> {
    let version = version()?;
    let iscc = require_iscc(iscc())?;
    let dist = root().join("dist");
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
    for arch in [Arch::X64, Arch::X86] {
        let exe = build_win7(arch)?;
        check_imports(&exe)?;
        let stage = dist.join(format!("stage-{}", arch.name()));
        std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
        for (from, to) in [
            (exe.clone(), "FeatherSend.exe"),
            (root().join("LICENSE"), "LICENSE.txt"),
            (root().join("NOTICE"), "NOTICE.txt"),
            (root().join(THIRD_PARTY), "THIRD-PARTY-LICENSES.txt"),
            (root().join("README.md"), "README.md"),
        ] {
            std::fs::copy(&from, stage.join(to)).map_err(|e| format!("{}: {e}", from.display()))?;
        }
        std::fs::write(
            stage.join("portable.txt"),
            "Settings and data are stored in the data folder next to FeatherSend.exe.\r\n",
        )
        .map_err(|e| e.to_string())?;
        let zip = dist.join(format!("FeatherSend-{version}-{}-portable.zip", arch.name()));
        powershell(&format!(
            "Compress-Archive -Path '{}\\*' -DestinationPath '{}' -Force",
            stage.display(),
            zip.display()
        ))?;
        run(Command::new(&iscc)
            .arg(format!("/DVersion={version}"))
            .arg(format!("/DArch={}", arch.name()))
            .arg(format!("/DSourceExe={}", exe.display()))
            .arg(format!("/O{}", dist.display()))
            .arg(root().join("installer").join("FeatherSend.iss")))?;
        let _ = std::fs::remove_dir_all(&stage);
    }
    powershell(&format!(
        "Get-ChildItem '{0}' -File | Where-Object Name -ne 'SHA256SUMS.txt' | ForEach-Object {{ '{{0}}  {{1}}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name }} | Set-Content -Encoding ascii '{0}\\SHA256SUMS.txt'",
        dist.display()
    ))?;
    let sums = dist.join("SHA256SUMS.txt");
    let text = std::fs::read_to_string(&sums).map_err(|e| e.to_string())?;
    std::fs::write(&sums, to_lf(&text)).map_err(|e| e.to_string())?;
    println!("artifacts in {}", dist.display());
    Ok(())
}

/// The notices of every library in the shipped executable, kept in the
/// repository so release builds need no extra tool.
const THIRD_PARTY: &str = "packaging/THIRD-PARTY-LICENSES.txt";

fn licenses() -> Result<()> {
    let about = Command::new("cargo").args(["about", "--version"]).output();
    if !about.is_ok_and(|o| o.status.success()) {
        return Err(
            "cargo-about is missing: cargo install cargo-about --locked --features cli".into()
        );
    }
    run(Command::new("cargo").current_dir(root()).args([
        "about",
        "generate",
        "packaging/about.hbs",
        "-o",
        THIRD_PARTY,
    ]))?;
    println!("wrote {THIRD_PARTY}");
    Ok(())
}

fn vendor_localsend(commit: Option<&str>) -> Result<()> {
    let commit = commit.ok_or("usage: cargo xtask vendor-localsend <commit>")?;
    let tmp = std::env::temp_dir().join("fs-localsend-src");
    let _ = std::fs::remove_dir_all(&tmp);
    run(Command::new("git")
        .args(["clone", "--filter=blob:none", "--no-checkout", "--sparse"])
        .arg("https://github.com/localsend/localsend.git")
        .arg(&tmp))?;
    run(Command::new("git").current_dir(&tmp).args(["sparse-checkout", "set", "packages/core"]))?;
    run(Command::new("git").current_dir(&tmp).args(["checkout", commit]))?;
    let dest = root().join("vendor").join("localsend-core");
    let keep = std::fs::read_to_string(dest.join("VENDORED.md")).unwrap_or_default();
    let license = std::fs::read(dest.join("LICENSE")).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dest);
    copy_dir(&tmp.join("packages").join("core"), &dest)?;
    std::fs::write(dest.join("LICENSE"), license).map_err(|e| e.to_string())?;
    let full = Command::new("git")
        .current_dir(&tmp)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    let full = String::from_utf8_lossy(&full.stdout).trim().to_string();
    let updated: String = keep
        .lines()
        .map(|l| {
            if l.starts_with("- Commit:") {
                format!("- Commit: `{full}`")
            } else if l.starts_with("- Source:") {
                format!(
                    "- Source: https://github.com/localsend/localsend/tree/{full}/packages/core"
                )
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dest.join("VENDORED.md"), updated + "\n").map_err(|e| e.to_string())?;
    run(Command::new("git").current_dir(root()).args([
        "apply",
        "--ignore-whitespace",
        "vendor/localsend-core.patch",
    ]))
    .map_err(|e| format!("our patch no longer applies, update it: {e}"))?;
    println!("vendored LocalSend core at {full}; review `git diff vendor/` and run the tests");
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "
  Section contains the following imports:

    KERNEL32.dll
             1400A0000 Import Address Table
                         0 time date stamp

                          2B5 GetSystemTimeAsFileTime
                          4C1 WaitOnAddress

    combase.dll
                           12 CoTaskMemFree

  Summary
";

    #[test]
    fn forbidden_import_detected() {
        let imports = parse_imports(SAMPLE);
        assert_eq!(imports.len(), 3);
        assert_eq!(
            violations(&imports),
            ["combase.dll!CoTaskMemFree", "kernel32.dll!WaitOnAddress"]
        );
    }

    #[test]
    fn clean_imports_pass() {
        let imports = vec![("kernel32.dll".to_string(), "GetSystemTimeAsFileTime".to_string())];
        assert!(violations(&imports).is_empty());
    }

    #[test]
    fn missing_inno_setup_is_an_error() {
        assert!(require_iscc(None).unwrap_err().contains("Inno Setup 6"));
        let found = PathBuf::from("ISCC.exe");
        assert_eq!(require_iscc(Some(found.clone())), Ok(found));
    }

    #[test]
    fn checksum_file_uses_lf() {
        assert_eq!(to_lf("aa  a.zip\r\nbb  b.exe\r\n"), "aa  a.zip\nbb  b.exe\n");
    }

    #[test]
    fn third_party_notices_cover_the_shipped_libraries() {
        let text = std::fs::read_to_string(root().join(THIRD_PARTY)).unwrap();
        for name in ["native-windows-gui", "ring", "rustls", "tokio", "hyper", "windows-sys"] {
            assert!(
                text.contains(&format!("  - {name} ")),
                "{name} missing: run cargo xtask licenses"
            );
        }
    }

    #[test]
    fn check_tag_matches_version() {
        let manifest = "[workspace]\nmembers=[]\n[workspace.package]\nversion = \"1.2.3\"\n";
        assert_eq!(workspace_version(manifest).as_deref(), Some("1.2.3"));
    }
}
