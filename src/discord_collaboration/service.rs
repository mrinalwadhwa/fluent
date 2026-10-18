use super::*;
use crate::slack_collaboration::{
    ClaudeStructuredSessionProvider, CodexAppServerProvider, KeychainSecretStore, ProviderRegistry,
    SecretStore, ServiceManager,
};
use std::process::Command;
use std::time::{Duration, Instant};
use tungstenite::{Message, stream::MaybeTlsStream};

pub const KEYCHAIN_SERVICE: &str = "dev.fluent.discord";
pub const BOT_TOKEN_ACCOUNT: &str = "bot-token";
const MISSING_SNAPSHOT_FAILURE: &str =
    "bound Discord conversation state is unavailable; restore its project snapshot from backup";
pub fn config_path(home: &Path) -> PathBuf {
    state_root(home).join("config.json")
}
pub fn write_config(home: &Path, config: &DiscordConfig) -> Result<()> {
    config.validate()?;
    write(&config_path(home), config)
}
pub fn read_config(home: &Path) -> Result<DiscordConfig> {
    let bytes = fs::read(config_path(home))
        .context("create the Discord runner config; see fluent discord setup")?;
    let config: DiscordConfig =
        serde_json::from_slice(&bytes).context("invalid Discord runner config")?;
    config.validate()?;
    Ok(config)
}
pub fn preflight_checkouts(config: &DiscordConfig) -> Result<()> {
    config.validate()?;
    if config
        .projects
        .values()
        .any(|p| !checkout_is_available(&p.checkout))
    {
        bail!(
            "configured Discord checkout is unavailable; update its local identity, branch, and commit"
        );
    }
    Ok(())
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OperationalState {
    pub connected: bool,
    pub last_connection: Option<String>,
    pub failure: Option<String>,
}
pub fn runner_status(config: &DiscordConfig, state: &OperationalState) -> String {
    let projects = config
        .projects
        .iter()
        .map(|(label, p)| {
            format!(
                "{label}: {} ({})",
                if checkout_is_available(&p.checkout) {
                    "available"
                } else {
                    "unavailable"
                },
                p.provider
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Discord server: {}; connection: {}; projects: {}{}",
        config.guild_id,
        if state.connected {
            "connected"
        } else {
            "disconnected"
        },
        projects,
        if state.failure.as_deref() == Some(MISSING_SNAPSHOT_FAILURE) {
            "; bound Discord conversation state is unavailable; restore its project snapshot from backup"
        } else if state.failure.is_some() {
            "; setup or connection needs attention (run fluent discord check)"
        } else {
            ""
        }
    )
}
pub fn read_operational_state(home: &Path) -> Result<OperationalState> {
    let mut state: OperationalState = read(&state_root(home).join("status.json"))?;
    // Inspect authoritative snapshots without writing conversation state. Keep this
    // diagnostic separate from Gateway status, which reconnects can overwrite.
    if config_path(home).exists() {
        let config = read_config(home)?;
        let index: Index = read(&state_root(home).join("index.json"))?;
        for binding in index.threads.values() {
            if let Some(project) = config.projects.get(&binding.project)
                && binding.checkout == project.checkout.identity
                && !thread_path(project, binding).try_exists()?
            {
                state.failure = Some(MISSING_SNAPSHOT_FAILURE.into());
                break;
            }
        }
    }
    if !crate::lease::is_leased(&state_root(home).join("runner.lock")) {
        state.connected = false;
    }
    Ok(state)
}
pub fn enable_runner(
    config: &DiscordConfig,
    manager: &mut impl ServiceManager,
    executable: &Path,
) -> Result<()> {
    preflight_checkouts(config)?;
    manager.install(executable)?;
    manager.start()
}
pub struct DiscordServiceManager {
    home: PathBuf,
    plist: PathBuf,
}
impl DiscordServiceManager {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.into(),
            plist: home.join("Library/LaunchAgents/dev.fluent.discord-runner.plist"),
        }
    }
    pub fn plist(&self) -> &Path {
        &self.plist
    }
    fn run(&self, verb: &str) -> Result<()> {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let output = Command::new("launchctl")
            .args([verb, &domain])
            .arg(&self.plist)
            .output()
            .context("invoke launchctl for Discord runner")?;
        if !output.status.success() {
            bail!("launchctl could not {verb} Discord runner; inspect the owner login service");
        }
        Ok(())
    }
}
impl ServiceManager for DiscordServiceManager {
    fn install(&mut self, executable: &Path) -> Result<()> {
        let escape = |text: &str| {
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        let path = escape(&executable.to_string_lossy());
        // launchd does not inherit the owner's interactive PATH.
        let user_bin = escape(&self.home.join(".local/bin").to_string_lossy());
        let xml = format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Label</key><string>dev.fluent.discord-runner</string><key>ProgramArguments</key><array><string>{path}</string><string>discord</string><string>run</string></array><key>EnvironmentVariables</key><dict><key>PATH</key><string>{user_bin}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>30</integer></dict></plist>"
        );
        fs::create_dir_all(self.plist.parent().unwrap())?;
        atomic_write(&self.plist, xml.as_bytes())?;
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        self.run("bootstrap")
    }
    fn restart(&mut self) -> Result<()> {
        self.stop()?;
        self.start()
    }
    fn stop(&mut self) -> Result<()> {
        self.run("bootout")
    }
    fn enabled(&self) -> bool {
        self.plist.is_file()
    }
}
pub fn configured_client(home: &Path) -> Result<(DiscordConfig, DiscordRest)> {
    let config = read_config(home)?;
    let token = KeychainSecretStore::default().read(KEYCHAIN_SERVICE, BOT_TOKEN_ACCOUNT).map_err(|_| anyhow::anyhow!("Discord bot credential is unavailable; add dev.fluent.discord / bot-token to the login Keychain"))?;
    let mut client = DiscordRest::new(token)?;
    client.preflight(&config)?;
    Ok((config, client))
}
/// Persist only normalized inputs. Interaction tokens never enter the inbox.
fn authorize_ingress(home: &Path, config: &DiscordConfig, input: &Input) -> Result<()> {
    if !admit_ingress(home, config, input)? {
        bail!("Discord actor or channel is not authorized");
    }
    Ok(())
}
pub(super) fn admit_ingress(home: &Path, config: &DiscordConfig, input: &Input) -> Result<bool> {
    config.validate()?;
    if input.guild != config.guild_id {
        return Ok(false);
    }
    let (parent, label) = match &input.kind {
        InputKind::Command { project, .. } => (input.channel.clone(), project.clone()),
        _ => {
            let index: Index = read(&state_root(home).join("index.json"))?;
            let Some(binding) = index.threads.get(&input.channel) else {
                return Ok(false);
            };
            (binding.parent.clone(), binding.project.clone())
        }
    };
    if !config
        .channels
        .get(&parent)
        .is_some_and(|p| p.contains(&label))
        || !config
            .projects
            .get(&label)
            .is_some_and(|p| p.participants.contains(&input.actor))
    {
        return Ok(false);
    }
    Ok(true)
}
pub fn persist_ingress(home: &Path, config: &DiscordConfig, input: &Input) -> Result<()> {
    authorize_ingress(home, config, input)?;
    snowflake(&input.id)?;
    let path = state_root(home)
        .join("inbox")
        .join(format!("{:x}.json", Sha256::digest(input.key().as_bytes())));
    if !path.exists() {
        write(&path, input)?;
    }
    Ok(())
}
fn normalize_attribution(home: &Path, config: &DiscordConfig, input: &mut Input) -> Result<()> {
    if input.actor.is_empty() {
        let index: Index = read(&state_root(home).join("index.json"))?;
        let binding = index
            .threads
            .get(&input.channel)
            .context("unbound correction")?;
        let project = config
            .projects
            .get(&binding.project)
            .context("project was revoked")?;
        let thread = read_thread(
            &project
                .checkout
                .path
                .join(".fluent/work/discord/threads")
                .join(format!("{}.json", binding.thread)),
        )?;
        input.actor = thread
            .inputs
            .iter()
            .find(|i| i.id == input.id && matches!(i.kind, InputKind::Message { .. }))
            .context("correction has no accepted original")?
            .actor
            .clone();
    }
    Ok(())
}
#[doc(hidden)]
pub fn process_inbox<P: ConversationProvider, T: DiscordTransport>(
    runner: &mut DiscordRunner<P, T>,
    live_inputs: &std::sync::Mutex<BTreeSet<String>>,
) -> Result<()> {
    let directory = state_root(&runner.home).join("inbox");
    fs::create_dir_all(&directory)?;
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_by_key(|path| {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Input>(&bytes).ok())
            .map(|input| (input.id.parse::<u64>().unwrap_or(0), input.key()))
    });
    let mut failure = None;
    for path in paths {
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // A damaged snapshot, input, or delivery must not block independent
        // conversations. Preserve its input and report failure after this pass.
        if let Err(error) = process_inbox_entry(runner, live_inputs, &path) {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
fn process_inbox_entry<P: ConversationProvider, T: DiscordTransport>(
    runner: &mut DiscordRunner<P, T>,
    live_inputs: &std::sync::Mutex<BTreeSet<String>>,
    path: &Path,
) -> Result<()> {
    let input = serde_json::from_slice(&fs::read(path)?)?;
    if !admit_ingress(&runner.home, &runner.config, &input)? {
        fs::remove_file(path)?;
        return Ok(());
    }
    let live = live_inputs.lock().unwrap().contains(&input.key());
    let result = if live {
        runner.ingest(input.clone())
    } else {
        runner.ingest_recovered(input.clone())
    };
    if let Err(error) = result {
        report_input_failure(runner, &input, &error)?;
        if !error.is::<RejectedInput>() {
            return Err(error);
        }
        // Keep a receipt whose effect needs reconciliation, but discard rejected approvals.
        let index: Index = read(&state_root(&runner.home).join("index.json"))?;
        if let Some(binding) = index.threads.get(&input.channel) {
            let project = &runner.config.projects[&binding.project];
            let thread = read_thread(
                &project
                    .checkout
                    .path
                    .join(".fluent/work/discord/threads")
                    .join(format!("{}.json", binding.thread)),
            )?;
            if thread
                .approvals
                .iter()
                .any(|a| a.input == input.key() && !a.completed)
            {
                return Err(error);
            }
        }
    }
    live_inputs.lock().unwrap().remove(&input.key());
    fs::remove_file(path)?;
    Ok(())
}

pub(super) fn report_input_failure<P: ConversationProvider, T: DiscordTransport>(
    runner: &mut DiscordRunner<P, T>,
    input: &Input,
    error: &anyhow::Error,
) -> Result<()> {
    let index: Index = read(&state_root(&runner.home).join("index.json"))?;
    let binding = match &input.kind {
        InputKind::Command { .. } => index.commands.get(&input.id),
        _ => index.threads.get(&input.channel),
    };
    let Some(binding) = binding else {
        return Ok(());
    };
    let project = &runner.config.projects[&binding.project];
    let path = project
        .checkout
        .path
        .join(".fluent/work/discord/threads")
        .join(format!("{}.json", binding.thread));
    let _lock = crate::lease::acquire_blocking(&state_root(&runner.home).join("state.lock"))?;
    let mut thread = read_thread(&path)?;
    let rejection = error.downcast_ref::<RejectedInput>();
    if rejection.is_some()
        && !thread
            .approvals
            .iter()
            .any(|a| a.input == input.key() && !a.completed)
    {
        // Keep rejected identities consumed even if the actor gains authority later.
        // Incomplete accepted effects retain their receipts for reconciliation.
        thread.rejected_inputs.insert(input.key());
    }
    let id = format!("{}:failure", input.key());
    if !thread.deliveries.iter().any(|d| d.id == id) {
        let text = if let Some(rejection) = rejection {
            format!("Input rejected: {rejection}. Request a new control from a project approver.")
        } else if let Some(provider) = error.downcast_ref::<crate::conversation::ProviderError>() {
            crate::conversation::provider_recovery_message(&provider.failure, false)
        } else if matches!(input.kind, InputKind::Approval { .. }) {
            "This approval could not complete; its input is preserved for recovery. The owner should check local Fluent status. Accepted effects will be reconciled before retrying.".into()
        } else {
            "This input is preserved but could not complete. The owner should check provider login and local Fluent status. An accepted or uncertain provider turn will not be repeated.".into()
        };
        thread.deliveries.push(Delivery {
            id,
            channel: binding.thread.clone(),
            text,
            message_id: None,
            approval: None,
        });
        write(&path, &thread)?;
    }
    super::rendering::prepare_deliveries(&mut thread, &project.checkout.path);
    write(&path, &thread)?;
    for i in 0..thread.deliveries.len() {
        if thread.deliveries[i].message_id.is_none() {
            thread.deliveries[i].message_id =
                Some(runner.transport.deliver(&thread.deliveries[i])?);
            write(&path, &thread)?;
        }
    }
    Ok(())
}
pub fn run_configured_runner(home: &Path) -> Result<()> {
    let config = read_config(home)?;
    let token = KeychainSecretStore::default()
        .read(KEYCHAIN_SERVICE, BOT_TOKEN_ACCOUNT)
        .map_err(|_| anyhow::anyhow!("Discord bot credential is unavailable"))?;
    let client = DiscordRest::new(token.clone())?;
    let providers: Vec<Box<dyn ConversationProvider>> = vec![
        Box::new(CodexAppServerProvider::new_scoped("codex".into())),
        Box::new(ClaudeStructuredSessionProvider::new_scoped("claude".into())),
    ];
    let provider = ProviderRegistry::new(providers)?;
    run_runner_with_dependencies(home, config, client, &token, provider, None)
}
/// Inject a bounded loopback Gateway without changing admission or worker behavior.
#[doc(hidden)]
pub struct GatewayFixture {
    pub url: String,
    pub connections: usize,
}
#[doc(hidden)]
pub fn run_runner_with_dependencies<P: ConversationProvider + Send + 'static>(
    home: &Path,
    config: DiscordConfig,
    mut client: DiscordRest,
    token: &str,
    provider: P,
    fixture: Option<GatewayFixture>,
) -> Result<()> {
    if let Some(fixture) = &fixture {
        let uri: tungstenite::http::Uri = fixture.url.parse()?;
        anyhow::ensure!(
            uri.scheme_str() == Some("ws")
                && uri.host().is_some_and(|host| host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()))
                && (1..=10).contains(&fixture.connections),
            "Gateway fixtures require a bounded loopback endpoint"
        );
    }
    let _lease = crate::lease::acquire(&state_root(home).join("runner.lock"))
        .context("Discord runner is already running")?;
    client.preflight(&config)?;
    let mut runner = DiscordRunner::new(home, config, provider, client.clone())?;
    let worker_home = home.to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = stop.clone();
    let connected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_connected = connected.clone();
    let live_inputs = std::sync::Arc::new(std::sync::Mutex::new(BTreeSet::<String>::new()));
    let worker_inputs = live_inputs.clone();
    let worker = std::thread::spawn(move || {
        let mut next_recovery = Instant::now();
        while !worker_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if worker_connected.load(std::sync::atomic::Ordering::Relaxed)
                && let Ok(config) = read_config(&worker_home)
            {
                runner.config = config;
                if Instant::now() >= next_recovery {
                    let _ = runner.recover();
                    next_recovery = Instant::now() + Duration::from_secs(30);
                }
                let _ = process_inbox(&mut runner, &worker_inputs);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    let result = run_gateway(
        home,
        client,
        token,
        &connected,
        &live_inputs,
        fixture.as_ref(),
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // A provider turn owns its own settlement; never detach it during shutdown.
    let _ = worker.join();
    result
}
fn run_gateway(
    home: &Path,
    client: DiscordRest,
    token: &str,
    connected: &std::sync::atomic::AtomicBool,
    live_inputs: &std::sync::Mutex<BTreeSet<String>>,
    fixture: Option<&GatewayFixture>,
) -> Result<()> {
    write(
        &state_root(home).join("status.json"),
        &OperationalState::default(),
    )?;
    let session_path = state_root(home).join("gateway.json");
    let mut session: GatewaySession = read(&session_path)?;
    let gateway = client.request("GET", "/gateway/bot", None)?;
    let fallback = gateway["url"]
        .as_str()
        .context("Discord omitted Gateway URL")?
        .to_owned();
    let mut connections = 0;
    loop {
        let url = gateway_url(session.resume_url.as_deref().unwrap_or(&fallback))?;
        if fixture.is_some_and(|fixture| connections >= fixture.connections) {
            return Ok(());
        }
        connections += 1;
        let connection = tungstenite::connect(fixture.map_or(url.as_str(), |f| f.url.as_str()));
        let (mut socket, _) = match connection {
            Ok(connection) => connection,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        match socket.get_mut() {
            MaybeTlsStream::Plain(stream) => {
                stream.set_read_timeout(Some(Duration::from_millis(200)))?
            }
            MaybeTlsStream::Rustls(stream) => stream
                .sock
                .set_read_timeout(Some(Duration::from_millis(200)))?,
            _ => bail!("unsupported Discord Gateway transport"),
        }
        let mut next_heartbeat = Instant::now() + Duration::from_secs(60);
        let mut fatal = None;
        loop {
            if Instant::now() >= next_heartbeat {
                let Ok(heartbeat) = session.heartbeat() else {
                    break;
                };
                if socket.send(Message::Text(heartbeat.to_string())).is_err() {
                    break;
                }
                next_heartbeat = Instant::now() + Duration::from_millis(session.heartbeat_ms);
            }
            let message = match socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            };
            if let Message::Close(frame) = message {
                if let Some(frame) = frame {
                    let code = u16::from(frame.code);
                    if matches!(code, 4004 | 4010..=4014) {
                        fatal = Some(gateway_close_failure(code));
                    }
                    if matches!(code, 4007 | 4009) {
                        session = GatewaySession::default();
                    }
                }
                break;
            }
            let Message::Text(text) = message else {
                continue;
            };
            let payload: serde_json::Value =
                serde_json::from_str(&text).context("invalid Discord Gateway JSON")?;
            // Persist normalized ingress before advancing the resume sequence.
            if let Some(mut input) = decode_dispatch(&payload)? {
                let config = read_config(home)?;
                let authorized = normalize_attribution(home, &config, &mut input)
                    .and_then(|_| authorize_ingress(home, &config, &input))
                    .is_ok();
                // Hold this small admission lock until acknowledgement resolves so the
                // worker cannot mistake a just-persisted live control for a recovered one.
                let mut live = live_inputs.lock().unwrap();
                if authorized {
                    persist_ingress(home, &config, &input)?;
                }
                if payload["t"] == "INTERACTION_CREATE" {
                    if let Some(token) = payload["d"]["token"].as_str() {
                        if client.acknowledge(&input.id, token, authorized).is_ok()
                            && authorized
                            && connected.load(std::sync::atomic::Ordering::Relaxed)
                        {
                            live.insert(input.key());
                        }
                    }
                } else if authorized {
                    live.insert(input.key());
                }
            }
            let responses = match session.handle(&payload, token) {
                Ok(r) => r,
                Err(_) => {
                    write(&session_path, &session)?;
                    break;
                }
            };
            for response in responses {
                socket
                    .send(Message::Text(response.to_string()))
                    .map_err(|_| anyhow::anyhow!("Discord Gateway send failed"))?;
            }
            if payload["op"] == 10 {
                next_heartbeat = Instant::now() + Duration::from_millis(session.heartbeat_ms / 2);
            }
            write(&session_path, &session)?;
            if payload["t"] == "READY" || payload["t"] == "RESUMED" {
                connected.store(true, std::sync::atomic::Ordering::Relaxed);
                write(
                    &state_root(home).join("status.json"),
                    &OperationalState {
                        connected: true,
                        last_connection: Some(chrono::Utc::now().to_rfc3339()),
                        failure: None,
                    },
                )?;
            }
        }
        write(&session_path, &session)?;
        connected.store(false, std::sync::atomic::Ordering::Relaxed);
        write(
            &state_root(home).join("status.json"),
            &OperationalState {
                connected: false,
                last_connection: None,
                failure: fatal.map(str::to_owned),
            },
        )?;
        if let Some(reason) = fatal {
            bail!(reason);
        }
        std::thread::sleep(if fixture.is_some() {
            Duration::from_millis(10)
        } else {
            Duration::from_secs(5)
        });
    }
}

/// Authenticate the selected provider using only this bound thread's private state.
pub fn provider_login(home: &Path, project: &str, thread: &str, provider: &str) -> Result<()> {
    provider_login_with_executable(home, project, thread, provider, Path::new(provider))
}
#[doc(hidden)]
pub fn provider_login_with_executable(
    home: &Path,
    label: &str,
    thread: &str,
    provider: &str,
    executable: &Path,
) -> Result<()> {
    snowflake(thread)?;
    let config = read_config(home)?;
    let index: Index = read(&state_root(home).join("index.json"))?;
    let binding = index
        .threads
        .get(thread)
        .context("Discord thread is not bound")?;
    let project = config
        .projects
        .get(label)
        .context("project is unavailable")?;
    if binding.project != label
        || binding.provider != provider
        || binding.checkout != project.checkout.identity
        || !checkout_is_available(&project.checkout)
    {
        bail!("provider login does not match the bound project and provider");
    }
    let draft = project
        .checkout
        .path
        .join(".fluent/work/discord/drafts")
        .join(thread);
    let private_home = draft.join(".provider-home");
    fs::create_dir_all(&private_home)?;
    let mut command = Command::new(executable);
    command
        .current_dir(&project.checkout.path)
        .env("HOME", private_home)
        .env_remove("DISCORD_BOT_TOKEN")
        .env_remove("DISCORD_TOKEN");
    match provider {
        "codex" => {
            let path = draft.join(".codex-home");
            fs::create_dir_all(&path)?;
            command
                .env("CODEX_HOME", path)
                .env_remove("CLAUDE_CONFIG_DIR")
                .arg("login");
        }
        "claude" => {
            let path = draft.join(".claude-home");
            fs::create_dir_all(&path)?;
            command
                .env("CLAUDE_CONFIG_DIR", path)
                .env_remove("CODEX_HOME");
        }
        _ => bail!("provider must be codex or claude"),
    }
    if !command.status()?.success() {
        bail!("provider login did not complete");
    }
    Ok(())
}
