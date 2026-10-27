#![cfg(feature = "test-support")]

use anyhow::{Context, Result};
use fluent::coder::{Coder, CoderSandbox};
use fluent::work_model::{WorkItem, WorkModelStore};
use fluent::work_task_executor::{node_worker_tester_for_test, node_worker_writer_for_test};
use serial_test::serial;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env(Vec<(String, Option<OsString>)>);
impl Env {
    fn set(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) {
        self.0.push((name.into(), std::env::var_os(name)));
        unsafe {
            std::env::set_var(name, value);
        }
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        for (name, old) in self.0.drain(..).rev() {
            unsafe {
                if let Some(value) = old {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn selected(tool: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join(tool))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("required test prerequisite: {tool} on PATH"))
        .canonicalize()
        .unwrap()
}

struct Fixture {
    _env: Env,
    _tmp: tempfile::TempDir,
    project: PathBuf,
    candidate: PathBuf,
    prefix: PathBuf,
    package: PathBuf,
    launcher: PathBuf,
    item: WorkItem,
}
impl Fixture {
    fn new(command: &str) -> Self {
        Self::prepare(command, false)
    }
    fn with_dependency(command: &str) -> Self {
        Self::prepare(command, true)
    }
    fn prepare(command: &str, dependency: bool) -> Self {
        let node = selected("node");
        let npm = selected("npm");
        let npm_package = npm.parent().unwrap().parent().unwrap();
        // The trusted-host check avoids the common profile's shared-temp grant.
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join(".scratch");
        fs::create_dir_all(&scratch).unwrap();
        let tmp = tempfile::Builder::new()
            .prefix("node-worker-")
            .tempdir_in(scratch)
            .unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let project = root.join("source");
        let candidate = root.join("candidate");
        let prefix = root.join("installer");
        let package = prefix.join("lib/node_modules/npm");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::create_dir_all(package.parent().unwrap()).unwrap();
        let copy = Command::new("/bin/cp")
            .args(["-R"])
            .arg(npm_package)
            .arg(&package)
            .output()
            .unwrap();
        assert!(
            copy.status.success(),
            "copy npm: {}",
            String::from_utf8_lossy(&copy.stderr)
        );
        symlink(node, prefix.join("bin/node")).unwrap();
        symlink(
            "../lib/node_modules/npm/bin/npm-cli.js",
            prefix.join("bin/npm"),
        )
        .unwrap();
        symlink(
            "../lib/node_modules/npm/bin/npx-cli.js",
            prefix.join("bin/npx"),
        )
        .unwrap();
        fs::create_dir_all(project.join(".fluent/expertise")).unwrap();
        fs::write(project.join(".fluent/expertise/INDEX.md"), "# Index\n").unwrap();
        fs::write(project.join(".gitignore"), "node_modules/\n.fluent/work/\n").unwrap();
        fs::write(
            project.join("package.json"),
            r#"{"name":"upgrade-fixture","version":"1.0.0","private":true}"#,
        )
        .unwrap();
        fs::write(
            project.join(".fluent/tester.yaml"),
            format!("commands:\n  - command: {command:?}\n    test_harness: shell-harness\n"),
        )
        .unwrap();
        executable(
            &project.join(".fluent/extract-tester-results"),
            "#!/bin/sh\necho '[]'\n",
        );
        if dependency {
            let packed = root.join("packed/package");
            fs::create_dir_all(&packed).unwrap();
            fs::write(
                packed.join("package.json"),
                r#"{"name":"local-value","version":"1.0.0","main":"index.js"}"#,
            )
            .unwrap();
            fs::write(
                packed.join("index.js"),
                "module.exports = require('./value.node');\n",
            )
            .unwrap();
            fs::write(
                packed.join("value.c"),
                r#"
                typedef struct napi_env__ *napi_env;
                typedef struct napi_value__ *napi_value;
                extern int napi_create_int32(napi_env, int, napi_value *);
                napi_value napi_register_module_v1(napi_env env, napi_value exports) {
                    napi_value value;
                    return napi_create_int32(env, 42, &value) == 0 ? value : exports;
                }
            "#,
            )
            .unwrap();
            let status = Command::new("/usr/bin/tar")
                .args(["-czf"])
                .arg(project.join("local-value.tgz"))
                .arg("-C")
                .arg(packed.parent().unwrap())
                .arg("package")
                .status()
                .unwrap();
            assert!(status.success());
            fs::write(
                project.join("package.json"),
                r#"{
              "name":"upgrade-fixture","version":"1.0.0","private":true,
              "dependencies":{"local-value":"file:local-value.tgz"},
              "scripts":{"postinstall":"node lifecycle.cjs", "test":"node test.cjs"}
            }"#,
            )
            .unwrap();
            fs::write(project.join("test.cjs"), "require('node:assert/strict').equal(require('local-value'), 42);\nrequire('node:assert/strict').ok(require('node:fs').existsSync('node_modules/lifecycle-marker'));\n").unwrap();
            fs::write(project.join("lifecycle.cjs"), r#"
                const flags = process.platform === 'darwin'
                  ? ['-bundle', '-undefined', 'dynamic_lookup'] : ['-shared', '-fPIC'];
                const compiler = require('node:child_process').spawnSync('cc', [...flags,
                  'node_modules/local-value/value.c', '-o', 'node_modules/local-value/value.node'],
                  {stdio: 'inherit'});
                if (compiler.error || compiler.signal || compiler.status !== 0) {
                  console.error('native module compiler failed:', {
                    status: compiler.status, signal: compiler.signal, error: compiler.error?.message
                  });
                  process.exit(compiler.status || 1);
                }
                console.log('native module compiler completed');
                require('node:fs').writeFileSync('node_modules/lifecycle-marker', process.env.npm_config_cache);
            "#).unwrap();
            let output = Command::new(prefix.join("bin/npm"))
                .args([
                    "install",
                    "--package-lock-only",
                    "--offline",
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                ])
                .env_clear()
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", prefix.join("bin").display()),
                )
                .env("HOME", root.join("setup-home"))
                .env("npm_config_userconfig", root.join("setup-user.npmrc"))
                .env("npm_config_globalconfig", root.join("setup-global.npmrc"))
                .env("npm_config_cache", root.join("setup-cache"))
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "generate local lockfile: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            fs::create_dir_all(project.join("node_modules/local-value")).unwrap();
            fs::write(
                project.join("node_modules/local-value/index.js"),
                "throw Error('source dependencies must never be used');\n",
            )
            .unwrap();
        }
        git(&project, &["init", "-q"]);
        git(&project, &["add", "."]);
        git(
            &project,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "Create Node fixture",
            ],
        );
        git(
            &project,
            &["worktree", "add", "--detach", candidate.to_str().unwrap()],
        );
        let mut item = WorkItem {
            id: "node-upgrade".into(),
            title: "Run npm".into(),
            ..Default::default()
        };
        item.add_initial_attempt("attempt-1").unwrap();
        WorkModelStore::new(&project)
            .create_work_item(&item)
            .unwrap();
        let home = root.join("operator");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".npmrc"),
            "//registry.example.test/:_authToken=fixture-secret\n",
        )
        .unwrap();
        let mut env = Env(Vec::new());
        env.set("HOME", &home);
        let path = format!(
            "{}:{}",
            prefix.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        env.set("PATH", path);
        let launcher = if std::env::var_os("FLUENT_NODE_WORKER_HOST").is_some() {
            PathBuf::from("/usr/bin/sandbox-exec")
        } else {
            env.set("FLUENT_TEST_HOST_SANDBOX_PREFLIGHT", "render-pass");
            let launcher = root.join("sandbox-double");
            executable(
                &launcher,
                "#!/bin/sh\ntest \"$1\" = -f || exit 91\ntest -s \"$2\" || exit 92\nshift 2\nexec \"$@\"\n",
            );
            env.set("FLUENT_TEST_FOCUSED_EVIDENCE_SANDBOX_EXEC", &launcher);
            launcher
        };
        Self {
            _env: env,
            _tmp: tmp,
            project,
            candidate,
            prefix,
            package,
            launcher,
            item,
        }
    }
    fn writer(&self, command: &str) -> Result<String> {
        node_worker_writer_for_test(&self.project, &self.candidate, &self.item, |sandbox| {
            Box::new(ShellCoder {
                sandbox,
                launcher: self.launcher.clone(),
                command: command.into(),
                node: self.prefix.join("bin/node"),
                npm_package: self.package.clone(),
            })
        })
        .with_context(|| self.npm_diagnostics())
    }
    fn npm_diagnostics(&self) -> String {
        // Retain the log contents before TempDir removes the private cache.
        let logs = self.project.join(format!(
            ".fluent/work/cache/writers/{}/{}/npm-cache/_logs",
            self.item.id, self.item.attempts[0].id
        ));
        let mut paths: Vec<_> = fs::read_dir(&logs)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
            .collect();
        paths.sort();
        let mut diagnostics = format!("npm diagnostics in {}", logs.display());
        for path in paths {
            diagnostics.push_str(&format!(
                "\n--- npm debug log: {} ---\n{}",
                path.display(),
                fs::read_to_string(&path).unwrap_or_else(|error| format!("read log: {error}"))
            ));
        }
        diagnostics
    }
    fn tester(&self) -> Result<String> {
        let artifact = self.project.join(".fluent/work/tester-probe");
        let (outcome, profile) =
            node_worker_tester_for_test(&self.project, &self.candidate, &artifact, &self.item)?;
        let results: fluent::tester::TesterResults =
            serde_json::from_slice(&fs::read(artifact.join("tester-results.json"))?)?;
        anyhow::ensure!(
            !results.commands.is_empty(),
            "Tester did not execute configured commands"
        );
        for command in &results.commands {
            anyhow::ensure!(
                command.exit_code == 0,
                "Tester command failed (exit status: {}): {}\nstdout:\n{}\nstderr:\n{}\n{}",
                command.exit_code,
                command.command,
                fs::read_to_string(artifact.join(&command.stdout_log))?,
                fs::read_to_string(artifact.join(&command.stderr_log))?,
                self.npm_diagnostics()
            );
        }
        anyhow::ensure!(
            outcome == fluent::tester::TesterOutcome::Passed,
            "Tester outcome: {outcome:?}; artifacts: {}\n{}",
            artifact.display(),
            self.npm_diagnostics()
        );
        Ok(profile)
    }
}
struct ShellCoder {
    sandbox: CoderSandbox,
    launcher: PathBuf,
    command: String,
    node: PathBuf,
    npm_package: PathBuf,
}
impl Coder for ShellCoder {
    fn run_interactive(
        &self,
        _: &str,
        _: &Path,
        _: &[String],
        _: &[(String, String)],
    ) -> Result<i32> {
        anyhow::bail!("unexpected interactive launch")
    }
    fn run(
        &self,
        _: &str,
        _: &str,
        working_dir: &Path,
        _: &[String],
        env: &[(String, String)],
        _: Option<&Path>,
    ) -> Result<i32> {
        let CoderSandbox::SeatbeltProfile(profile) = &self.sandbox else {
            anyhow::bail!("Writer must retain confinement");
        };
        let output = Command::new(&self.launcher)
            .args(["-f", profile, "/bin/sh", "-c", &self.command])
            .current_dir(working_dir)
            .envs(env.iter().cloned())
            .output()
            .context("launch Writer command")?;
        let diagnostics = if output.status.success() {
            String::new()
        } else {
            // Probe the same profile after failure without retrying the command
            // or printing inherited environment values (which may hold tokens).
            let probe = Command::new(&self.launcher)
                .args(["-f", profile])
                .arg(&self.node)
                .arg("-e")
                .arg(include_str!("fixtures/node-worker-spawn-probe.cjs"))
                .arg(&self.npm_package)
                .current_dir(working_dir)
                .envs(env.iter().cloned())
                .output();
            match probe {
                Ok(probe) => format!(
                    "\nNode spawn diagnostics: {}\n{}\n{}",
                    probe.status,
                    String::from_utf8_lossy(&probe.stdout),
                    String::from_utf8_lossy(&probe.stderr)
                ),
                Err(error) => format!("\nNode spawn diagnostics could not launch: {error}"),
            }
        };
        anyhow::ensure!(
            output.status.success(),
            "Writer command failed ({}): {}\nstdout:\n{}\nstderr:\n{}{}",
            output.status,
            self.command,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            diagnostics
        );
        Ok(0)
    }
}

#[test]
#[serial]
fn node_installer_symlinks_run_in_writer_and_tester() {
    let f =
        Fixture::new("npm --version && npx --version && node -e 'console.log(process.version)'");
    for profile in [
        f.writer("npm --version && npx --version && node -e 'console.log(process.version)'")
            .unwrap(),
        f.tester().unwrap(),
    ] {
        assert!(
            profile.contains(&format!(
                "(allow file-map-executable (subpath \"{}\"))",
                f.candidate.display()
            )),
            "candidate native modules need executable mapping"
        );
        assert!(
            profile.contains(&format!(
                "(allow file-read*  (subpath \"{}\"))",
                f.package.display()
            )),
            "missing npm package read grant"
        );
        assert!(
            profile.contains(&format!(
                "(deny file-write* (subpath \"{}\"))",
                f.package.display()
            )),
            "installed npm must be read-only"
        );
    }
}

#[test]
#[serial]
fn node_tool_grants_exclude_unrelated_files_and_credentials() {
    let host = std::env::var_os("FLUENT_NODE_WORKER_HOST").is_some();
    let command = if host {
        "npm --version && node node_modules/probe.cjs"
    } else {
        "npm --version"
    };
    let f = Fixture::new(command);
    let sibling = f.prefix.join("lib/node_modules/unrelated/secret");
    fs::create_dir_all(sibling.parent().unwrap()).unwrap();
    fs::write(&sibling, "unrelated package").unwrap();
    let operator = PathBuf::from(std::env::var_os("HOME").unwrap());
    fs::create_dir_all(operator.join(".npm")).unwrap();
    let operator_cache = operator.join(".npm/retained");
    fs::write(&operator_cache, "operator cache").unwrap();
    let tool = f.package.join("package.json");
    let tool_before = fs::read(&tool).unwrap();
    let denied_reads = serde_json::to_string(&[sibling, operator.join(".npmrc")]).unwrap();
    let denied_writes = serde_json::to_string(&[tool.clone(), operator_cache.clone()]).unwrap();
    fs::create_dir_all(f.candidate.join("node_modules")).unwrap();
    fs::write(f.candidate.join("node_modules/probe.cjs"), format!(r#"
        const fs = require('node:fs');
        for (const p of {denied_reads}) {{
          let denied = false;
          try {{ fs.readFileSync(p); }} catch (e) {{ if (['EPERM','EACCES'].includes(e.code)) denied = true; else throw e; }}
          if (!denied) throw Error('unexpected read: '+p);
        }}
        for (const p of {denied_writes}) {{
          let denied = false;
          try {{ const fd=fs.openSync(p, fs.constants.O_WRONLY); fs.closeSync(fd); }} catch (e) {{ if (['EPERM','EACCES'].includes(e.code)) denied = true; else throw e; }}
          if (!denied) throw Error('unexpected write: '+p);
        }}
    "#)).unwrap();
    for profile in [f.writer(command).unwrap(), f.tester().unwrap()] {
        for outside in [
            f.prefix.clone(),
            f.prefix.join("lib/node_modules"),
            f.prefix.join("lib/node_modules/unrelated"),
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(".npmrc"),
        ] {
            assert!(
                !profile.contains(&format!(
                    "(allow file-read*  (subpath \"{}\"))",
                    outside.display()
                )),
                "overbroad grant: {}",
                outside.display()
            );
        }
        assert!(
            profile.contains(&format!(
                "(allow file-read*  (subpath \"{}\"))",
                f.package.display()
            )),
            "positive control: selected npm must be admitted"
        );
    }
    assert_eq!(fs::read(tool).unwrap(), tool_before);
    assert_eq!(
        fs::read_to_string(operator_cache).unwrap(),
        "operator cache"
    );
}

#[test]
#[serial]
fn node_path_search_traverses_directories_without_reading_contents() {
    let command = "npm test";
    let mut f = Fixture::new(command);
    let private = f._tmp.path().join("unselected");
    let bin = private.join("store/tools/bin");
    let link = private.join("linked-profile");
    let profiles = private.join(".local/state/profiles");
    let current = profiles.join("profile");
    let generation = profiles.join("profile-195-link");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&profiles).unwrap();
    // Neither the lexical PATH ancestors nor the final canonical ancestors
    // include these intermediate profile links and their parents.
    symlink(".local/state/profiles/profile", &link).unwrap();
    symlink("profile-195-link", &current).unwrap();
    symlink(bin.parent().unwrap(), &generation).unwrap();
    let secret = bin.join("unrelated-tool");
    fs::write(&secret, "unselected tool contents").unwrap();
    let path = format!(
        "{}:{}",
        link.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    f._env.set("PATH", &path);
    fs::write(
        f.candidate.join("package.json"),
        r#"{"name":"path-probe","version":"1.0.0","scripts":{"test":"node path-probe.cjs"}}"#,
    )
    .unwrap();
    let expected_node = f.prefix.join("bin/node").canonicalize().unwrap();
    fs::write(
        f.candidate.join("path-probe.cjs"),
        format!(
            r#"
        const assert = require('node:assert/strict');
        const fs = require('node:fs');
        const {{ spawnSync }} = require('node:child_process');
        assert.equal(process.execPath, {expected_node});
        assert.ok(process.env.PATH.endsWith({path}));
        for (const [tool, args] of [['sh', ['-c', 'exit 0']], ['cc', ['--version']]]) {{
          const child = spawnSync(tool, args, {{stdio: 'inherit'}});
          assert.ifError(child.error);
          assert.equal(child.status, 0, tool);
        }}
        const missing = spawnSync('fluent-missing-path-probe', [], {{stdio: 'pipe'}});
        assert.equal(missing.error?.code, 'ENOENT');
        assert.equal(missing.status, null);
        if (process.env.FLUENT_NODE_WORKER_HOST) {{
          for (const directory of {denied_directories}) {{
            const secret = directory + '/unrelated-tool';
            for (const operation of [
              () => fs.readFileSync(secret),
              () => fs.readdirSync(directory),
              () => fs.openSync(secret, fs.constants.O_WRONLY),
            ]) {{
              assert.throws(operation, error => ['EPERM', 'EACCES'].includes(error.code));
            }}
          }}
        }}
    "#,
            expected_node = serde_json::to_string(&expected_node).unwrap(),
            path = serde_json::to_string(&path).unwrap(),
            denied_directories = serde_json::to_string(&[
                bin.clone(),
                link.join("bin"),
                current.join("bin"),
                generation.join("bin"),
            ])
            .unwrap(),
        ),
    )
    .unwrap();
    git(&f.candidate, &["add", "package.json", "path-probe.cjs"]);
    git(
        &f.candidate,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Probe inherited tool search",
        ],
    );
    for profile in [f.writer(command).unwrap(), f.tester().unwrap()] {
        for directory in [
            &private,
            &private.join("store"),
            &private.join("store/tools"),
            &bin,
            &link,
            &link.join("bin"),
            &private.join(".local"),
            &private.join(".local/state"),
            &profiles,
            &current,
            &generation,
        ] {
            assert!(
                profile.contains(&format!(
                    "(allow file-read-metadata (literal \"{}\"))",
                    directory.display()
                )),
                "missing PATH traversal: {}",
                directory.display()
            );
            for operation in [
                "file-read*",
                "file-read-data",
                "file-write*",
                "file-map-executable",
            ] {
                assert!(
                    !profile
                        .lines()
                        .any(|line| line.contains(&format!("(allow {operation} "))
                            && line.contains(&format!("\"{}\"", directory.display()))),
                    "PATH traversal must not grant {operation}: {}",
                    directory.display()
                );
            }
        }
    }
    assert_eq!(
        fs::read_to_string(secret).unwrap(),
        "unselected tool contents"
    );
}

#[test]
#[serial]
fn invalid_node_toolchain_reports_exact_prerequisite() {
    let f = Fixture::new("npm --version");
    let assert_prerequisite = |path: &Path, detail: &str| {
        for result in [f.writer("true"), f.tester()] {
            let error = result
                .err()
                .expect("invalid selected tools must prevent launch");
            let message = format!("{error:#}");
            assert!(message.contains("Node prerequisite"), "{message}");
            assert!(message.contains(path.to_str().unwrap()), "{message}");
            assert!(message.contains(detail), "{message}");
        }
    };
    // An intact alternate npm still exists later on PATH; never select it.
    let launcher = f.prefix.join("bin/npm");
    let original = fs::read_link(&launcher).unwrap();
    fs::remove_file(&launcher).unwrap();
    symlink("../lib/node_modules/missing/bin/npm-cli.js", &launcher).unwrap();
    assert_prerequisite(&launcher, "resolve selected npm");
    fs::remove_file(&launcher).unwrap();
    symlink("npm", &launcher).unwrap();
    assert_prerequisite(&launcher, "resolve selected npm");
    fs::remove_file(&launcher).unwrap();
    symlink(&original, &launcher).unwrap();

    let node = f.prefix.join("bin/node");
    let node_target = fs::read_link(&node).unwrap();
    fs::remove_file(&node).unwrap();
    symlink("missing-node", &node).unwrap();
    assert_prerequisite(&node, "resolve selected node");
    fs::remove_file(&node).unwrap();
    symlink(node_target, &node).unwrap();

    let escape = f.package.join("node_modules/escaped-config");
    let secret = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".npmrc");
    symlink(&secret, &escape).unwrap();
    assert_prerequisite(&escape, "escapes");
    fs::remove_file(&escape).unwrap();
    let manifest = f.package.join("package.json");
    let original_manifest = fs::read(&manifest).unwrap();
    fs::write(&manifest, r#"{"name":"unrelated"}"#).unwrap();
    assert_prerequisite(&f.package.join("bin/npm-cli.js"), "not an npm package");
    fs::write(&manifest, original_manifest).unwrap();
    assert_eq!(
        fs::read_to_string(secret).unwrap(),
        "//registry.example.test/:_authToken=fixture-secret\n"
    );
}

#[test]
#[serial]
fn npm_cache_is_private_to_attempt_and_shared_with_tester() {
    let command = "mkdir -p node_modules && npm config get cache > node_modules/tester-cache && test -f \"$(npm config get cache)/writer-marker\"";
    let mut f = Fixture::new(command);
    f.writer("mkdir -p node_modules && npm config get cache > node_modules/writer-cache && node -e 'const fs=require(\"fs\"); const c=process.env.npm_config_cache; if (!c || !c.includes(\"/node-upgrade/attempt-1/npm-cache\")) throw Error(\"missing private npm cache\"); fs.writeFileSync(c+\"/writer-marker\",\"retained\")'").unwrap();
    f.writer("test -f \"$(npm config get cache)/writer-marker\"")
        .unwrap();
    let tester_profile = f.tester().unwrap();
    let expected = f
        .project
        .join(".fluent/work/cache/writers/node-upgrade/attempt-1/npm-cache");
    assert_eq!(
        fs::read_to_string(f.candidate.join("node_modules/writer-cache"))
            .unwrap()
            .trim(),
        expected.to_str().unwrap()
    );
    assert_eq!(
        fs::read(f.candidate.join("node_modules/writer-cache")).unwrap(),
        fs::read(f.candidate.join("node_modules/tester-cache")).unwrap()
    );
    let operator = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(!tester_profile.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        operator.join(".npm").display()
    )));
    assert!(!expected.join(".npmrc").exists());
    f.item.add_initial_attempt("attempt-2").unwrap();
    WorkModelStore::new(&f.project)
        .write_work_item(&f.item)
        .unwrap();
    f.item.attempts.swap(0, 1);
    f.writer("node -e 'const fs=require(\"fs\"); const c=process.env.npm_config_cache; if (!c.endsWith(\"/attempt-2/npm-cache\") || fs.existsSync(c+\"/writer-marker\")) throw Error(\"cache ownership mixed\")'").unwrap();
    assert_eq!(
        fs::read_to_string(expected.join("writer-marker")).unwrap(),
        "retained"
    );
}

#[test]
#[serial]
fn clean_worktree_installs_and_tests_without_host_node_modules() {
    let command = "npm ci --offline --no-audit --no-fund && npm test";
    let f = Fixture::with_dependency(command);
    assert!(!f.candidate.join("node_modules").exists());
    let lockfile = fs::read(f.candidate.join("package-lock.json")).unwrap();
    f.writer(command).unwrap();
    assert_eq!(
        fs::read_to_string(f.candidate.join("node_modules/local-value/index.js")).unwrap(),
        "module.exports = require('./value.node');\n"
    );
    let cache = f
        .project
        .join(".fluent/work/cache/writers/node-upgrade/attempt-1/npm-cache");
    assert!(cache.join("_cacache").is_dir());
    assert_eq!(
        fs::read_to_string(f.candidate.join("node_modules/lifecycle-marker")).unwrap(),
        cache.to_str().unwrap()
    );
    fs::remove_dir_all(f.candidate.join("node_modules")).unwrap();
    f.tester().unwrap();
    assert!(
        f.candidate
            .join("node_modules/local-value/index.js")
            .is_file()
    );
    assert_eq!(
        fs::read(f.candidate.join("package-lock.json")).unwrap(),
        lockfile
    );
    assert!(
        fs::read_to_string(f.project.join("node_modules/local-value/index.js"))
            .unwrap()
            .contains("must never be used")
    );
    assert!(
        git(
            &f.candidate,
            &["status", "--porcelain", "--untracked-files=no"]
        )
        .is_empty()
    );
}

#[test]
#[serial]
fn existing_060_node_project_runs_without_reinit_or_config_rewrite() {
    let mut f = Fixture::with_dependency("npm ci --offline --no-audit --no-fund && npm test");
    let files: &[(&str, &[u8])] = &[
        (
            ".fluent/config.yaml",
            include_bytes!("fixtures/node-worker-060/config.yaml"),
        ),
        (
            ".fluent/work/items/saved-node-work.json",
            include_bytes!("fixtures/node-worker-060/work-item.json"),
        ),
        (
            ".fluent/work/attempts/saved-node-work/attempt-1.json",
            include_bytes!("fixtures/node-worker-060/attempt.json"),
        ),
        (
            ".fluent/work/tasks/saved-node-work/attempt-1/attempt-1-write-1.json",
            include_bytes!("fixtures/node-worker-060/task.json"),
        ),
    ];
    for (relative, bytes) in files {
        let path = f.project.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fs::write(f.candidate.join(".fluent/config.yaml"), files[0].1).unwrap();
    git(&f.candidate, &["add", ".fluent/config.yaml"]);
    git(
        &f.candidate,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Retain saved coder settings",
        ],
    );
    f.item = WorkModelStore::new(&f.project)
        .read_work_item_snapshot("saved-node-work")
        .unwrap();
    assert_eq!(f.item.attempts.len(), 1);
    assert_eq!(
        f.item.instructions.as_deref(),
        Some("Run the saved Node project commands.")
    );
    let configured = fluent::config::from_config(&f.project).unwrap();
    assert_eq!(configured.write_model.as_deref(), Some("claude-sonnet-4-6"));
    let paths = [
        ".fluent/config.yaml",
        ".fluent/tester.yaml",
        "package-lock.json",
    ];
    let before: Vec<_> = paths
        .iter()
        .map(|p| fs::read(f.candidate.join(p)).unwrap())
        .collect();
    f.writer("npm ci --offline --no-audit --no-fund && npm test")
        .unwrap();
    fs::remove_dir_all(f.candidate.join("node_modules")).unwrap();
    f.tester().unwrap();
    for (relative, bytes) in files {
        assert_eq!(
            fs::read(f.project.join(relative)).unwrap(),
            *bytes,
            "saved state changed: {relative}"
        );
    }
    for (path, bytes) in paths.iter().zip(before) {
        assert_eq!(
            fs::read(f.candidate.join(path)).unwrap(),
            bytes,
            "candidate config changed: {path}"
        );
    }
}

#[test]
#[serial]
fn npm_lifecycle_failures_retain_status_and_diagnostics() {
    let mut env = Env(Vec::new());
    env.set(
        "FLUENT_SPAWN_PROBE_SECRET",
        "private-value-must-not-be-logged",
    );
    let command =
        "PATH=\"$PWD/failing-tools:$PATH\" npm ci --offline --no-audit --no-fund && npm test";
    let f = Fixture::with_dependency(command);
    fs::create_dir(f.candidate.join("failing-tools")).unwrap();
    executable(
        &f.candidate.join("failing-tools/cc"),
        "#!/bin/sh\necho 'intentional compiler failure' >&2\nexit 23\n",
    );
    git(&f.candidate, &["add", "failing-tools/cc"]);
    git(
        &f.candidate,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Fail the lifecycle script",
        ],
    );
    for role in ["Writer", "Tester"] {
        let result = if role == "Writer" {
            f.writer(command)
        } else {
            f.tester()
        };
        let error = format!(
            "{:#}",
            result.expect_err("lifecycle failure must reject the worker")
        );
        assert!(error.contains("exit status: 23"), "{role}: {error}");
        assert!(
            error.contains("intentional compiler failure"),
            "{role}: {error}"
        );
        assert!(
            error.contains("native module compiler failed"),
            "{role}: {error}"
        );
        assert!(error.contains("status: 23"), "{role}: {error}");
        assert!(error.contains("npm debug log"), "{role}: {error}");
        assert!(error.contains("verbose exit 23"), "{role}: {error}");
        assert!(
            !error.contains("private-value-must-not-be-logged"),
            "{role}"
        );
        if role == "Writer" {
            assert!(error.contains("Node spawn diagnostics:"), "{error}");
            assert!(
                error.contains(r#""case":"npm-shell","status":0"#),
                "{error}"
            );
            assert!(
                error.contains(r#""case":"absolute-shell","status":0"#),
                "{error}"
            );
            assert!(
                error.contains(r#""case":"missing-executable","error":"ENOENT""#),
                "{error}"
            );
            assert!(
                error.contains(r#""case":"missing-path-command","error":"ENOENT""#),
                "{error}"
            );
        }
        assert!(!f.candidate.join("node_modules/lifecycle-marker").exists());
    }
}

#[test]
#[serial]
fn writer_signal_failures_retain_status() {
    let f = Fixture::new("true");
    let error = format!("{:#}", f.writer("kill -TERM $$").unwrap_err());
    assert!(error.contains("signal: 15"), "{error}");
    assert!(error.contains("kill -TERM $$"), "{error}");
}

#[test]
#[serial]
fn failing_npm_test_is_not_accepted() {
    let f = Fixture::with_dependency("npm ci --offline --no-audit --no-fund && npm test");
    fs::write(
        f.candidate.join("test.cjs"),
        "require('node:assert/strict').equal(require('local-value'), 99);\n",
    )
    .unwrap();
    git(&f.candidate, &["add", "test.cjs"]);
    git(
        &f.candidate,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Require a failing dependency value",
        ],
    );
    for role in ["Writer", "Tester"] {
        let result = if role == "Writer" {
            f.writer("npm ci --offline --no-audit --no-fund && npm test")
        } else {
            fs::remove_dir_all(f.candidate.join("node_modules")).unwrap();
            f.tester()
        };
        let error = format!(
            "{:#}",
            result.expect_err("npm assertions must reject the worker")
        );
        assert!(error.contains("ERR_ASSERTION"), "{role}: {error}");
        assert!(error.contains("42 !== 99"), "{role}: {error}");
        assert!(error.contains("exit status: 1"), "{role}: {error}");
        assert!(
            f.candidate.join("node_modules/lifecycle-marker").exists(),
            "{role}: {error}"
        );
    }
}
