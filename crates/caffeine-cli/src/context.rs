//! Shared state for one CLI invocation.

use std::cell::OnceCell;

use caffeine_core::config::ConfigFile;
use caffeine_core::power::PowerBackend;
use caffeine_core::session::SessionStore;
use caffeine_core::{Error, Paths, Result};
use caffeine_platform::BackendOptions;

use crate::cli::GlobalArgs;
use crate::output::Output;

pub struct Ctx {
    pub paths: Paths,
    pub config: ConfigFile,
    /// Set when the configuration file could not be loaded and defaults are
    /// in use.
    pub config_error: Option<Error>,
    pub out: Output,
    pub store: SessionStore,
    pub global: GlobalArgs,
    backend: OnceCell<Box<dyn PowerBackend>>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl Ctx {
    pub fn load(global: &GlobalArgs) -> Result<Ctx> {
        let mut paths = Paths::resolve()?;
        if let Some(file) = &global.config_file {
            paths = paths.with_config_file(file.clone());
        }
        let (config, config_error) = ConfigFile::load_or_default(&paths.config_file);
        let out = Output::new(global, config.config().general.color);
        let store = SessionStore::new(&paths);
        Ok(Ctx {
            paths,
            config,
            config_error,
            out,
            store,
            global: global.clone(),
            backend: OnceCell::new(),
        })
    }

    pub fn backend_options(&self) -> BackendOptions {
        let advanced = &self.config.config().advanced;
        BackendOptions {
            backend: advanced.backend.clone(),
            experimental: advanced.experimental.clone(),
            data_dir: self.paths.data_dir.clone(),
        }
    }

    /// The native backend, created on first use.
    pub fn backend(&self) -> Result<&dyn PowerBackend> {
        if let Some(b) = self.backend.get() {
            return Ok(b.as_ref());
        }
        let b = caffeine_platform::native(&self.backend_options())?;
        let _ = self.backend.set(b);
        Ok(self.backend.get().expect("just set").as_ref())
    }

    /// Warn (once, at startup) when running on default settings because the
    /// configuration file is broken.
    pub fn warn_config_problems(&self) {
        if let Some(err) = &self.config_error {
            self.out.warn(format!("{err}"));
            self.out
                .warn("continuing with default settings; the file was not modified");
        }
    }
}
