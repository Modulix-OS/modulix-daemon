//! Shared shape for "install X / uninstall X by name" commands.

/// Generate a pair of [`Command`](super::Command) implementations for
/// installing and uninstalling something identified by a single name.
///
/// `$install`/`$uninstall` (D-Bus method names, e.g. `InstallPackage`) become
/// the generated struct names; `$kind` is the noun used in the log message,
/// the stubbed library call (`install-$kind`/`uninstall-$kind`) and the
/// returned status string.
///
/// Requires `Command` and `Error` to be in scope at the call site.
#[macro_export]
macro_rules! lifecycle_commands {
    ($install:ident, $uninstall:ident, $kind:literal) => {
        pub struct $install;

        #[async_trait::async_trait]
        impl Command for $install {
            fn name(&self) -> &'static str {
                stringify!($install)
            }

            async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
                let [name] = arguments else {
                    unreachable!(concat!(stringify!($install), " takes exactly one argument"))
                };

                tracing::info!(name = %name, concat!("installing ", $kind));

                #[cfg(not(debug_assertions))]
                println!(concat!("install-", $kind, " {}"), name);

                Ok(format!(concat!($kind, " {} installed"), name))
            }
        }

        pub struct $uninstall;

        #[async_trait::async_trait]
        impl Command for $uninstall {
            fn name(&self) -> &'static str {
                stringify!($uninstall)
            }

            async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
                let [name] = arguments else {
                    unreachable!(concat!(stringify!($uninstall), " takes exactly one argument"))
                };

                tracing::info!(name = %name, concat!("uninstalling ", $kind));

                #[cfg(not(debug_assertions))]
                println!(concat!("uninstall-", $kind, " {}"), name);

                Ok(format!(concat!($kind, " {} uninstalled"), name))
            }
        }
    };
}
