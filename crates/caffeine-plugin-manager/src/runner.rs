//! Running plugin commands and diagnostics.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::Value;

use caffeine_core::{Error, Result};
use caffeine_plugin_api::plugin::{HostInfo, Initialize};
use caffeine_plugin_api::protocol::{HostMessage, MAX_LINE_BYTES, PluginMessage, encode};
use caffeine_plugin_api::{Check, Host, Invocation, LogLevel, PROTOCOL_VERSION, Permission};

use crate::{Manager, Origin, PluginEntry, TrustStatus, env, ungranted_permissions};

const INIT_TIMEOUT: Duration = Duration::from_secs(15);
const DIAGNOSE_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A command to run.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub json: bool,
}

impl Manager {
    /// Check that `name` may run now and build its initialization data.
    pub fn prepare(&self, name: &str) -> Result<(&PluginEntry, Initialize)> {
        let entry = self.require(name)?;
        if !entry.enabled {
            return Err(Error::plugin(format!("plugin \"{name}\" is not enabled"))
                .with_hint(format!("review it with `caffeine plugin info {name}`, then `caffeine plugin enable {name}`")));
        }
        if entry.has_errors() {
            return Err(Error::plugin(format!(
                "plugin \"{name}\" cannot run: {}",
                entry
                    .problems
                    .iter()
                    .filter(|p| p.is_error())
                    .map(|p| p.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        match entry.trust {
            TrustStatus::BuiltIn | TrustStatus::Trusted => {}
            TrustStatus::NotTrusted => {
                return Err(
                    Error::plugin(format!("plugin \"{name}\" is enabled but not trusted"))
                        .with_hint(format!(
                            "run `caffeine plugin enable {name}` to review and trust it"
                        )),
                );
            }
            TrustStatus::Changed => {
                return Err(Error::plugin(format!(
                    "plugin \"{name}\" changed since you enabled it; refusing to run it"
                ))
                .with_hint(format!("review it with `caffeine plugin info {name}`, then re-run `caffeine plugin enable {name}`")));
            }
        }
        let ungranted = ungranted_permissions(self, entry);
        if !ungranted.is_empty() {
            return Err(Error::plugin(format!(
                "plugin \"{name}\" now asks for permissions you did not grant: {}",
                ungranted
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .with_hint(format!(
                "re-run `caffeine plugin enable {name}` to review them"
            )));
        }
        let data_dir = self.data_dir_for(name);
        caffeine_core::Paths::ensure_dir(&data_dir)?;
        let init = Initialize {
            host: HostInfo::current(self.host_version().clone()),
            settings: self.resolved_settings(entry),
            permissions: entry.manifest.plugin.permissions.clone(),
            data_dir,
        };
        Ok((entry, init))
    }

    /// Run a command of an enabled plugin, forwarding its output to `host`.
    /// Returns the command's exit code.
    pub fn run(&self, name: &str, request: &RunRequest, host: &mut dyn Host) -> Result<i32> {
        let (entry, init) = self.prepare(name)?;
        if entry.manifest.command(&request.command).is_none() {
            let known: Vec<_> = entry
                .manifest
                .commands
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            return Err(Error::usage(format!(
                "plugin \"{name}\" has no command \"{}\" (commands: {})",
                request.command,
                known.join(", ")
            )));
        }
        let invocation = Invocation {
            command: request.command.clone(),
            args: request.args.clone(),
            cwd: request.cwd.clone(),
            json: request.json,
        };
        if entry.origin == Origin::BuiltIn {
            let builtin = self.builtin(name).expect("built-in entries have factories");
            let mut plugin = (builtin.factory)();
            let result = plugin
                .initialize(&init)
                .and_then(|()| plugin.invoke(&invocation, host));
            plugin.shutdown();
            return result.map_err(plugin_error(name));
        }
        let mut process = PluginProcess::spawn(self, entry, &request.cwd)?;
        let result = process
            .initialize(init, entry)
            .and_then(|()| process.invoke(invocation, host));
        process.shutdown();
        result
    }

    /// Ask an enabled plugin to diagnose itself.
    pub fn diagnose(&self, name: &str) -> Result<Vec<Check>> {
        let (entry, init) = self.prepare(name)?;
        if entry.origin == Origin::BuiltIn {
            let builtin = self.builtin(name).expect("built-in entries have factories");
            let mut plugin = (builtin.factory)();
            let result = plugin.initialize(&init).map(|()| plugin.diagnose());
            plugin.shutdown();
            return result.map_err(plugin_error(name));
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| init.data_dir.clone());
        let mut process = PluginProcess::spawn(self, entry, &cwd)?;
        let result = process
            .initialize(init, entry)
            .and_then(|()| process.diagnose());
        process.shutdown();
        result
    }
}

fn plugin_error(name: &str) -> impl Fn(caffeine_plugin_api::PluginError) -> Error + '_ {
    move |e| {
        let mut err = Error::plugin(format!("{name}: {}", e.message));
        if let Some(h) = e.hint {
            err = err.with_hint(h);
        }
        err
    }
}

/// A running third-party plugin.
struct PluginProcess {
    name: String,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<std::result::Result<String, String>>,
}

impl PluginProcess {
    fn spawn(
        manager: &Manager,
        entry: &PluginEntry,
        cwd: &std::path::Path,
    ) -> Result<PluginProcess> {
        let name = entry.name().to_string();
        let exe = entry
            .executable
            .as_ref()
            .expect("runnable third-party plugins have executables");
        let dir = entry
            .dir
            .as_ref()
            .expect("third-party plugins have directories");
        let spec = entry
            .manifest
            .executable
            .as_ref()
            .expect("checked by discovery");
        let inherit = entry.manifest.has_permission(Permission::EnvInherit);
        let env = env::build(
            std::env::vars_os(),
            inherit,
            &name,
            dir,
            &manager.data_dir_for(&name),
        );
        log::debug!(
            "starting plugin {name}: {} {:?} (cwd {}, {} environment variables{})",
            exe.display(),
            spec.args,
            cwd.display(),
            env.len(),
            if inherit { ", inherited" } else { "" }
        );
        let mut child = Command::new(exe)
            .args(&spec.args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::io(format!("starting plugin \"{name}\" ({})", exe.display()), e))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = Vec::new();
                match (&mut reader)
                    .take(MAX_LINE_BYTES as u64 + 1)
                    .read_until(b'\n', &mut buf)
                {
                    Ok(0) => break,
                    Ok(n) if n > MAX_LINE_BYTES => {
                        let _ =
                            tx.send(Err("plugin sent a protocol line longer than 4 MiB".into()));
                        break;
                    }
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("reading plugin output: {e}")));
                        break;
                    }
                }
            }
        });
        Ok(PluginProcess {
            name,
            child,
            stdin,
            lines: rx,
        })
    }

    fn send(&mut self, message: &HostMessage) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::plugin(format!("plugin \"{}\" closed its input", self.name)))?;
        stdin
            .write_all(encode(message).as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| {
                Error::plugin(format!(
                    "plugin \"{}\" stopped reading requests: {e}",
                    self.name
                ))
            })
    }

    /// The next protocol message. Unknown message types are skipped; stray
    /// non-JSON output is forwarded to `host` as text when one is given.
    fn recv(
        &mut self,
        timeout: Option<Duration>,
        mut host: Option<&mut dyn Host>,
    ) -> Result<PluginMessage> {
        loop {
            let next = match timeout {
                Some(t) => self.lines.recv_timeout(t),
                None => self
                    .lines
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            let line = match next {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => return Err(Error::plugin(format!("{}: {e}", self.name))),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Error::plugin(format!(
                        "plugin \"{}\" did not answer within {}s",
                        self.name,
                        timeout.map_or(0, |t| t.as_secs())
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait().map(|s| s.to_string()).unwrap_or_default();
                    return Err(Error::plugin(format!(
                        "plugin \"{}\" exited without answering ({status})",
                        self.name
                    )));
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => {
                    log::debug!("plugin {} wrote non-protocol output", self.name);
                    if let Some(h) = host.as_deref_mut() {
                        h.print(&line);
                    }
                    continue;
                }
            };
            match serde_json::from_value::<PluginMessage>(value.clone()) {
                Ok(m) => return Ok(m),
                Err(e) => {
                    let kind = value.get("type").and_then(Value::as_str).unwrap_or("?");
                    log::debug!("ignoring message \"{kind}\" from plugin {}: {e}", self.name);
                }
            }
        }
    }

    fn initialize(&mut self, init: Initialize, entry: &PluginEntry) -> Result<()> {
        self.send(&HostMessage::initialize(PROTOCOL_VERSION, init))?;
        loop {
            match self.recv(Some(INIT_TIMEOUT), None)? {
                PluginMessage::Ready {
                    protocol,
                    name,
                    version,
                } => {
                    if protocol.major != PROTOCOL_VERSION.major {
                        return Err(Error::plugin(format!(
                            "plugin \"{}\" speaks protocol {protocol}, expected {PROTOCOL_VERSION}",
                            self.name
                        )));
                    }
                    if name != entry.name() || version != entry.manifest.plugin.version {
                        return Err(Error::plugin(format!(
                            "the executable identifies as {name} {version}, but the manifest declares {} {}",
                            entry.name(),
                            entry.manifest.plugin.version
                        ))
                        .with_hint("rebuild the plugin so its executable matches caffeine-plugin.toml"));
                    }
                    return Ok(());
                }
                PluginMessage::Failed { error } => return Err(plugin_error(&self.name)(error)),
                PluginMessage::Log { level, message } => forward_log(&self.name, level, &message),
                other => return Err(self.unexpected(&other)),
            }
        }
    }

    fn invoke(&mut self, invocation: Invocation, host: &mut dyn Host) -> Result<i32> {
        self.send(&HostMessage::Invoke(invocation))?;
        loop {
            match self.recv(None, Some(&mut *host))? {
                PluginMessage::Log { level, message } => host.log(level, &message),
                PluginMessage::Output { text } => host.print(&text),
                PluginMessage::Data { value } => host.data(value),
                PluginMessage::Result { exit_code } => return Ok(exit_code),
                PluginMessage::Failed { error } => return Err(plugin_error(&self.name)(error)),
                other => return Err(self.unexpected(&other)),
            }
        }
    }

    fn diagnose(&mut self) -> Result<Vec<Check>> {
        self.send(&HostMessage::Diagnose)?;
        loop {
            match self.recv(Some(DIAGNOSE_TIMEOUT), None)? {
                PluginMessage::Diagnostics { checks } => return Ok(checks),
                PluginMessage::Failed { error } => return Err(plugin_error(&self.name)(error)),
                PluginMessage::Log { level, message } => forward_log(&self.name, level, &message),
                other => return Err(self.unexpected(&other)),
            }
        }
    }

    fn unexpected(&self, message: &PluginMessage) -> Error {
        Error::plugin(format!(
            "plugin \"{}\" sent an unexpected message: {}",
            self.name,
            encode(message).trim_end()
        ))
    }

    /// Ask the plugin to exit, killing it if it does not.
    fn shutdown(&mut self) {
        let _ = self.send(&HostMessage::Shutdown);
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + EXIT_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        log::warn!("plugin {} exited with {status}", self.name);
                    }
                    return;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    log::warn!(
                        "plugin {} did not exit after shutdown; terminating it",
                        self.name
                    );
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn forward_log(plugin: &str, level: LogLevel, message: &str) {
    let level = match level {
        LogLevel::Error => log::Level::Error,
        LogLevel::Warn => log::Level::Warn,
        LogLevel::Info => log::Level::Info,
        LogLevel::Debug => log::Level::Debug,
        LogLevel::Trace => log::Level::Trace,
    };
    log::log!(target: "caffeine::plugin", level, "{plugin}: {message}");
}
