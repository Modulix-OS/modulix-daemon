//! Shared shape for "install X / uninstall X by name(s)" commands backed by
//! a blocking `fn(config_dir: &str, names: &[&str]) -> mx::Result<()>` pair
//! from `modulix-core-utils`.

/// Generate a pair of [`Command`](super::Command) implementations for
/// installing and uninstalling one or more things identified by name.
///
/// `$install`/`$uninstall` (D-Bus method names, e.g. `InstallPackage`) become
/// the generated struct names; `$kind` is the noun used in the log message
/// and the returned status string; `$install_fn`/`$uninstall_fn` are the
/// blocking `modulix-core-utils` calls run via `spawn_blocking`, skipped when
/// [`crate::dry_run::is_dry_run`] is true.
///
/// `arguments` holds one name per element, forwarded in a single call.
///
/// Requires `Command` and `Error` to be in scope at the call site.
#[macro_export]
macro_rules! lifecycle_commands {
    ($install:ident, $uninstall:ident, $kind:literal, $install_fn:path, $uninstall_fn:path) => {
        pub struct $install;

        #[async_trait::async_trait]
        impl Command for $install {
            fn name(&self) -> &'static str {
                stringify!($install)
            }

            async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
                let names = arguments.join(", ");

                tracing::info!(names = %names, concat!("installing ", $kind));

                if !$crate::dry_run::is_dry_run() {
                    let owned: Vec<String> = arguments.iter().map(|s| s.to_string()).collect();
                    tokio::task::spawn_blocking(move || {
                        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                        $install_fn($crate::config_dir::config_dir(), &refs)
                    })
                    .await
                    .map_err(|e| Error::CoreUtils(e.to_string()))?
                    .map_err(|e| Error::CoreUtils(e.to_string()))?;
                }

                Ok(format!(concat!($kind, " {} installed"), names))
            }
        }

        pub struct $uninstall;

        #[async_trait::async_trait]
        impl Command for $uninstall {
            fn name(&self) -> &'static str {
                stringify!($uninstall)
            }

            async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
                let names = arguments.join(", ");

                tracing::info!(names = %names, concat!("uninstalling ", $kind));

                if !$crate::dry_run::is_dry_run() {
                    let owned: Vec<String> = arguments.iter().map(|s| s.to_string()).collect();
                    tokio::task::spawn_blocking(move || {
                        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                        $uninstall_fn($crate::config_dir::config_dir(), &refs)
                    })
                    .await
                    .map_err(|e| Error::CoreUtils(e.to_string()))?
                    .map_err(|e| Error::CoreUtils(e.to_string()))?;
                }

                Ok(format!(concat!($kind, " {} uninstalled"), names))
            }
        }
    };
}
