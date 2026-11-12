//! Android: compiles the Java half of the bridge (`android/java`) into a
//! dex that `src/android` loads at run time, as the packaged app carries
//! NSH's own classes only. Needs `javac` (JAVA_HOME) and the Android SDK
//! (ANDROID_HOME: a platform's android.jar, build-tools' d8 and lambda
//! stubs).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The oldest Android the dex runs on (InMemoryDexClassLoader).
const MIN_API: &str = "26";
const JAVA_DIR: &str = "android/java";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={JAVA_DIR}");
    println!("cargo:rerun-if-env-changed=ANDROID_HOME");
    println!("cargo:rerun-if-env-changed=ANDROID_SDK_ROOT");
    println!("cargo:rerun-if-env-changed=JAVA_HOME");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let sdk = env::var_os("ANDROID_HOME")
        .or_else(|| env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set ANDROID_HOME to the Android SDK"));
    let android_jar = newest(&sdk.join("platforms"), "android.jar")
        .unwrap_or_else(|| panic!("no platform with android.jar in {}", sdk.display()));
    let d8 = newest(&sdk.join("build-tools"), "d8")
        .unwrap_or_else(|| panic!("no build-tools with d8 in {}", sdk.display()));
    // Lambdas compile against these stubs; d8 desugars them.
    let lambda_stubs = d8.with_file_name("core-lambda-stubs.jar");

    let classes = out.join("classes");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes).expect("create the classes directory");
    let mut sources = Vec::new();
    files_with(Path::new(JAVA_DIR), "java", &mut sources);
    run(Command::new(javac())
        .args([
            "-source",
            "8",
            "-target",
            "8",
            "-nowarn",
            "-encoding",
            "UTF-8",
        ])
        .arg("-bootclasspath")
        .arg(&android_jar)
        .arg("-classpath")
        .arg(&lambda_stubs)
        .arg("-d")
        .arg(&classes)
        .args(&sources));

    let mut class_files = Vec::new();
    files_with(&classes, "class", &mut class_files);
    run(Command::new(d8)
        .args(["--release", "--min-api", MIN_API, "--lib"])
        .arg(&android_jar)
        .arg("--output")
        .arg(&out)
        .args(&class_files));
}

fn javac() -> PathBuf {
    env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/javac"))
        .unwrap_or_else(|| PathBuf::from("javac"))
}

/// `name` in the subdirectory of `dir` with the highest version.
fn newest(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.join(name).exists())
        .max_by_key(|path| version(path))
        .map(|path| path.join(name))
}

/// The numbers in a directory name: `android-36.1` → [36, 1].
fn version(path: &Path) -> Vec<u32> {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn files_with(dir: &Path, extension: &str, found: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for path in entries.filter_map(|entry| entry.ok().map(|e| e.path())) {
        if path.is_dir() {
            files_with(&path, extension, found);
        } else if path.extension().is_some_and(|e| e == extension) {
            found.push(path);
        }
    }
}

fn run(command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("cannot run {command:?}: {e}"));
    assert!(status.success(), "{command:?} failed: {status}");
}
