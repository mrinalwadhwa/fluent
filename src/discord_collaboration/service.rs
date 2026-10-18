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
        if state.failure.is_some() {
            "; setup or connection needs attention (run fluent discord check)"
        } else {
            ""
        }
    )
}
pub fn read_operational_state(home: &Path) -> Result<OperationalState> {
    read(&state_root(home).join("status.json"))
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
pub fn persist_ingress(home: &Path, config: &DiscordConfig, input: &Input) -> Result<()> {
    config.validate()?;
    if input.guild != config.guild_id {
        bail!("Discord server is not authorized");
    }
    let (parent, label) = match &input.kind {
        InputKind::Command { project, .. } => (input.channel.clone(), project.clone()),
        _ => {
            let index: Index = read(&state_root(home).join("index.json"))?;
            let binding = index
                .threads
                .get(&input.channel)
                .context("Discord thread is not bound")?;
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
        bail!("Discord actor or channel is not authorized");
    }
    snowflake(&input.id)?;
    let path = state_root(home)
        .join("inbox")
        .join(format!("{}.json", input.id));
    if !path.exists() {
        write(&path, input)?;
    }
    Ok(())
}
fn process_inbox<P: ConversationProvider, T: DiscordTransport>(
    runner: &mut DiscordRunner<P, T>,
) -> Result<()> {
    let directory = state_root(&runner.home).join("inbox");
    fs::create_dir_all(&directory)?;
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_by_key(|path| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    });
    for path in paths {
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let input = serde_json::from_slice(&fs::read(&path)?)?;
        runner.ingest(input)?;
        fs::remove_file(path)?;
    }
    Ok(())
}
pub fn run_configured_runner(home: &Path) -> Result<()> {
    let _lease = crate::lease::acquire(&state_root(home).join("runner.lock"))
        .context("Discord runner is already running")?;
    let (config, client) = configured_client(home)?;
    let token = KeychainSecretStore::default()
        .read(KEYCHAIN_SERVICE, BOT_TOKEN_ACCOUNT)
        .map_err(|_| anyhow::anyhow!("Discord bot credential is unavailable"))?;
    let providers: Vec<Box<dyn ConversationProvider>> = vec![
        Box::new(CodexAppServerProvider::new_scoped("codex".into())),
        Box::new(ClaudeStructuredSessionProvider::new_scoped("claude".into())),
    ];
    let provider = ProviderRegistry::new(providers)?;
    let mut runner = DiscordRunner::new(home, config, provider, client.clone())?;
    let worker_home = home.to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = stop.clone();
    let connected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_connected = connected.clone();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(std::sync::atomic::Ordering::Relaxed) {
            if worker_connected.load(std::sync::atomic::Ordering::Relaxed)
                && let Ok(config) = read_config(&worker_home)
            {
                runner.config = config;
                let _ = process_inbox(&mut runner);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    let result = run_gateway(home, client, &token, &connected);
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
) -> Result<()> {
    let session_path = state_root(home).join("gateway.json");
    let mut session: GatewaySession = read(&session_path)?;
    let gateway = client.request("GET", "/gateway/bot", None)?;
    let fallback = gateway["url"]
        .as_str()
        .context("Discord omitted Gateway URL")?
        .to_owned();
    loop {
        let url = session.resume_url.as_deref().unwrap_or(&fallback);
        if !url.starts_with("wss://gateway.discord.gg")
            && !url.starts_with("wss://gateway-us-east1-b.discord.gg")
        {
            bail!("Discord returned an untrusted Gateway URL");
        }
        let connection =
            tungstenite::connect(format!("{}/?v=10&encoding=json", url.trim_end_matches('/')));
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
            if let Some(input) = decode_dispatch(&payload)? {
                let config = read_config(home)?;
                let accepted = persist_ingress(home, &config, &input).is_ok();
                if payload["t"] == "INTERACTION_CREATE" {
                    if let Some(token) = payload["d"]["token"].as_str() {
                        let _ = client.acknowledge(&input.id, token, accepted);
                    }
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
        std::thread::sleep(Duration::from_secs(5));
    }
}
