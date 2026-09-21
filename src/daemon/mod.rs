//! The daemon's own D-Bus interface: `org.modulix.Daemon`.
//!
//! This is the privileged write side of the daemon (as opposed to the
//! read-only `org.modulix.Store1`, served from `crate::store`). Every method
//! here mutates the system's NixOS configuration and triggers a rebuild of
//! it, so a call only returns once that rebuild has finished — this can take
//! **minutes**, not milliseconds; callers must not treat these as
//! fire-and-forget or apply a short timeout.
//!
//! Commands are added as methods on the [`Daemon`] interface impl below.
//! Most of them (`Install*`/`Uninstall*` for packages, modules and plugins)
//! delegate to a [`crate::command::Command`] implementation looked up by
//! name in the registry built by [`crate::command::registry`] and stored in
//! `Daemon::commands`; dispatch is name-based string matching done in
//! `Daemon::run`. `SetOptions` is the one exception: it does not go
//! through the `Command` registry at all, calling
//! `crate::command::setting::apply_option`/`crate::command::setting::apply_list`
//! directly instead (see that method's doc for why this matters).
//!
//! Every method first checks polkit authorization (see [`crate::polkit`])
//! for the caller identified by the message header — the D-Bus policy
//! (`org.modulix.Daemon.conf`) only controls who can *reach* these methods,
//! not who is allowed to use them. [`crate::polkit::check`] prompts the
//! caller's polkit agent for authentication if needed and, when the caller
//! declines or authorization is otherwise denied, returns an error (mapped
//! to a D-Bus `Failed` reply via [`crate::error::Error::CoreUtils`]) instead
//! of running the command — no partial work happens in that case.
//!
//! Whether the underlying `modulix-core-utils` library call actually runs
//! (as opposed to being logged and skipped) is controlled by
//! [`crate::dry_run::is_dry_run`], which defaults to skipping it in debug
//! builds and running it in release builds unless overridden by the
//! `MX_DAEMON_DRY_RUN` environment variable. In dry-run mode the method
//! still returns its normal human-readable status string, as if the
//! transaction had happened.

use crate::command::setting::{Setting, apply_list, apply_option};
use crate::command::{self, Command};
use crate::polkit::{self, ACTION_INSTALL, ACTION_REMOVE};

/// Well-known bus name the daemon owns on the system bus.
///
/// Used both to acquire the name when building the connection
/// (`crate::main`) and by clients to address the daemon
/// (`default_service` in `modulix-store-client`'s `DaemonProxy`).
pub const BUS_NAME: &str = "org.modulix.Daemon";

/// Object path at which the [`Daemon`] interface (and the sibling
/// `org.modulix.Store1` interface) is served.
pub const OBJECT_PATH: &str = "/org/modulix/Daemon";

/// The `org.modulix.Daemon` interface implementation: the privileged write
/// side of the daemon. See the module documentation for the shared
/// dispatch, authorization and dry-run behaviour across its methods.
pub struct Daemon {
    /// Every registered [`Command`], as built by [`crate::command::registry`]
    /// when the [`Daemon`] is constructed. Looked up by name in `Self::run`
    /// to dispatch an incoming D-Bus method call; not consulted by
    /// `SetOptions`, which bypasses the `Command` trait entirely.
    commands: Vec<Box<dyn Command>>,
}

impl Daemon {
    /// Builds the interface, wiring in every command from
    /// [`crate::command::registry`].
    ///
    /// # Returns
    /// A ready-to-serve [`Daemon`] with its command table populated. Does
    /// not touch D-Bus itself; the caller still has to `serve_at` it on a
    /// [`zbus::Connection`] (see `crate::main`).
    pub fn new() -> Self {
        Self {
            commands: command::registry(),
        }
    }

    /// Shared dispatch path for every `Install*`/`Uninstall*` method:
    /// authorize the caller, look up the named [`Command`], run it, and
    /// invalidate the `Store1` installed-listing cache on success.
    ///
    /// # Parameters
    /// * `connection` - the bus connection the incoming call arrived on;
    ///   forwarded to [`crate::polkit::check`], which needs it to query the
    ///   polkit authority.
    /// * `header` - the D-Bus message header of the incoming call, used by
    ///   [`crate::polkit::check`] to identify the calling process/user as
    ///   the polkit `Subject`.
    /// * `action_id` - the polkit action id to check (one of
    ///   [`crate::polkit::ACTION_INSTALL`]/[`crate::polkit::ACTION_REMOVE`]).
    /// * `name` - the D-Bus method name (e.g. `"InstallPackage"`), used both
    ///   to find the matching [`Command`] in `Self::commands` by
    ///   [`Command::name`] and, via its `Install`/`Uninstall` prefix, to
    ///   decide whether to invalidate the installed-listing cache below.
    /// * `arguments` - the command's arguments in order, forwarded verbatim
    ///   to [`Command::execute`].
    ///
    /// # Pre-conditions
    /// `name` should match the `name()` of exactly one entry in
    /// `Self::commands`; every current caller passes a literal that does.
    ///
    /// # Returns
    /// The command's human-readable status string on success.
    ///
    /// # Post-conditions
    /// On success, if `name` starts with `"Install"` or `"Uninstall"`, the
    /// `Store1` installed-listing cache is invalidated via
    /// `crate::store::invalidate_installed` so it does not keep serving the
    /// pre-write answer for up to `INSTALLED_CACHE_TTL` (see
    /// `crate::store`) — long enough for GNOME Software to show "Install"
    /// again on the app it has just installed. This is unconditional for
    /// every command reachable through this path, since all of them are
    /// named `Install*`/`Uninstall*`.
    ///
    /// # Errors
    /// Returns an error, and skips both execution and cache invalidation,
    /// when: the polkit check denies authorization or itself fails (caller
    /// declined the prompt, or the authority call errored); `name` matches
    /// no registered command ([`zbus::fdo::Error::UnknownMethod`]); or
    /// [`Command::execute`] itself fails (mapped from
    /// [`crate::error::Error`] via `Into`) — this includes the
    /// `modulix-core-utils` call failing, in which case that library is
    /// responsible for leaving the configuration unchanged.
    async fn run(
        &self,
        connection: &zbus::Connection,
        header: &zbus::message::Header<'_>,
        action_id: &str,
        name: &str,
        arguments: &[&str],
    ) -> zbus::fdo::Result<String> {
        polkit::check(connection, header, action_id).await?;

        let command = self
            .commands
            .iter()
            .find(|command| command.name() == name)
            .ok_or_else(|| zbus::fdo::Error::UnknownMethod(name.to_string()))?;

        let result = command.execute(arguments).await;

        if result.is_ok() && (name.starts_with("Install") || name.starts_with("Uninstall")) {
            crate::store::invalidate_installed();
        }

        result.map_err(Into::into)
    }
}

impl Default for Daemon {
    /// Equivalent to [`Daemon::new`]; satisfies the conventional
    /// `Default`-from-`new` pattern.
    ///
    /// # Returns
    /// A ready-to-serve [`Daemon`], identical to [`Daemon::new`]'s.
    fn default() -> Self {
        Self::new()
    }
}

#[zbus::interface(name = "org.modulix.Daemon")]
impl Daemon {
    /// Adds one or more system packages (plain nixpkgs attributes, not
    /// Modulix modules) to the system configuration and rebuilds it.
    ///
    /// D-Bus signature: `InstallPackage(as names) -> (s)`.
    ///
    /// # Parameters
    /// * `names` - nixpkgs attribute paths to install, all forwarded in a
    ///   single `modulix-core-utils` call/transaction.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_INSTALL`] polkit
    /// action; the caller's polkit agent may prompt for authentication.
    ///
    /// # Returns
    /// A human-readable status line of the form `"package <names> installed"`
    /// (`<names>` being `names` joined with `", "`), once the rebuild has
    /// completed.
    ///
    /// # Post-conditions
    /// The call blocks for the whole `nixos-rebuild` triggered by the
    /// underlying library call, which can take **minutes**. Unless
    /// [`crate::dry_run::is_dry_run`] is true (the default in debug builds),
    /// the packages belong to the active generation on success. In dry-run
    /// mode nothing is actually installed and the same status string is
    /// returned as if it had been. On success the `Store1` installed-listing
    /// cache is invalidated (see `Daemon::run`).
    ///
    /// # Errors
    /// A D-Bus error reply is returned, and no rebuild is attempted, if the
    /// caller declines or is otherwise denied polkit authorization. A D-Bus
    /// error reply is also returned if the underlying `modulix-core-utils`
    /// call fails, in which case the configuration is left unchanged (see
    /// `Daemon::run`).
    async fn install_package(
        &self,
        names: Vec<&str>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.run(
            connection,
            &header,
            ACTION_INSTALL,
            "InstallPackage",
            &names,
        )
        .await
    }

    /// Removes one or more system packages from the system configuration
    /// and rebuilds it.
    ///
    /// D-Bus signature: `UninstallPackage(as names) -> (s)`.
    ///
    /// # Parameters
    /// * `names` - nixpkgs attribute paths to remove, all forwarded in a
    ///   single `modulix-core-utils` call/transaction. Names that were not
    ///   declared are silently ignored, not an error.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_REMOVE`] polkit
    /// action; the caller's polkit agent may prompt for authentication.
    ///
    /// # Returns
    /// A human-readable status line of the form
    /// `"package <names> uninstalled"`, once the rebuild has completed.
    ///
    /// # Post-conditions
    /// Same blocking (minutes-long `nixos-rebuild`), dry-run and
    /// cache-invalidation behaviour as `Self::install_package`.
    ///
    /// # Errors
    /// Same failure modes as `Self::install_package`: denied polkit
    /// authorization, or the underlying `modulix-core-utils` call failing
    /// (configuration left unchanged).
    async fn uninstall_package(
        &self,
        names: Vec<&str>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.run(
            connection,
            &header,
            ACTION_REMOVE,
            "UninstallPackage",
            &names,
        )
        .await
    }

    /// Enables one or more Modulix modules in the system configuration and
    /// rebuilds it.
    ///
    /// D-Bus signature: `InstallModule(as names) -> (s)`.
    ///
    /// # Parameters
    /// * `names` - module keys to enable. Unlike packages, each name is
    ///   rebuilt in its own `nixos-rebuild` transaction, sequentially (see
    ///   `crate::command::module::InstallModule`). A meta-module (e.g.
    ///   `programs.games`) is expanded to its sub-modules inside
    ///   `modulix-core-utils`; the daemon still only forwards the name(s) it
    ///   received.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_INSTALL`] polkit
    /// action; the caller's polkit agent may prompt for authentication.
    ///
    /// # Returns
    /// A human-readable status line of the form `"module <names> installed"`,
    /// once every name has been processed.
    ///
    /// # Post-conditions
    /// The call blocks for as many minutes-long `nixos-rebuild`s as there
    /// are names in `names`, run one after another. Because each name gets
    /// its own transaction, a failure partway through leaves the names
    /// already processed enabled. Unless [`crate::dry_run::is_dry_run`] is
    /// true (the default in debug builds), nothing is actually enabled and
    /// the same status string is returned as if it had been. On success the
    /// `Store1` installed-listing cache is invalidated (see `Daemon::run`).
    ///
    /// # Errors
    /// A D-Bus error reply is returned, and no rebuild is attempted, if the
    /// caller declines or is otherwise denied polkit authorization. A D-Bus
    /// error reply is also returned if the underlying `modulix-core-utils`
    /// call fails for any name, aborting the remaining names in the list.
    async fn install_module(
        &self,
        names: Vec<&str>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.run(connection, &header, ACTION_INSTALL, "InstallModule", &names)
            .await
    }

    /// Disables one or more Modulix modules in the system configuration and
    /// rebuilds it.
    ///
    /// D-Bus signature: `UninstallModule(as names) -> (s)`.
    ///
    /// # Parameters
    /// * `names` - module keys to disable, one `nixos-rebuild` transaction
    ///   each, sequentially. Same meta-module expansion as
    ///   `Self::install_module`.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_REMOVE`] polkit
    /// action; the caller's polkit agent may prompt for authentication.
    ///
    /// # Returns
    /// A human-readable status line of the form
    /// `"module <names> uninstalled"`, once every name has been processed.
    ///
    /// # Post-conditions
    /// Same blocking (one minutes-long `nixos-rebuild` per name), partial-
    /// failure, dry-run and cache-invalidation behaviour as
    /// `Self::install_module`.
    ///
    /// # Errors
    /// Same failure modes as `Self::install_module`: denied polkit
    /// authorization, or the underlying `modulix-core-utils` call failing
    /// for one of the names (aborting the rest).
    async fn uninstall_module(
        &self,
        names: Vec<&str>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.run(
            connection,
            &header,
            ACTION_REMOVE,
            "UninstallModule",
            &names,
        )
        .await
    }

    /// Enables one plugin of a module and rebuilds the system.
    ///
    /// D-Bus signature: `InstallPlugin(s module, s plugin) -> (s)`.
    ///
    /// # Parameters
    /// * `module` - module key owning the plugin; its metadata
    ///   (`ModuleInfo::plugins_namespace`) supplies the nixpkgs namespace the
    ///   plugin is resolved under, so the caller never passes that namespace
    ///   directly.
    /// * `plugin` - bare plugin name within `module`.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_INSTALL`] polkit
    /// action; the caller's polkit agent may prompt for authentication.
    ///
    /// # Returns
    /// A human-readable status line of the form
    /// `"plugin <plugin> installed for module <module>"`, once the rebuild
    /// has completed.
    ///
    /// # Post-conditions
    /// The call blocks for the whole minutes-long `nixos-rebuild`. Unless
    /// [`crate::dry_run::is_dry_run`] is true (the default in debug builds),
    /// `module` is enabled as a side effect if it was not already, and the
    /// plugin belongs to the active generation on success. In dry-run mode
    /// nothing is actually installed and the same status string is returned
    /// as if it had been. On success the `Store1` installed-listing cache is
    /// invalidated (see `Daemon::run`).
    ///
    /// # Errors
    /// A D-Bus error reply is returned, and no rebuild is attempted, if the
    /// caller declines or is otherwise denied polkit authorization, if
    /// `module`'s metadata cannot be resolved, or if `module` has no plugin
    /// namespace. A D-Bus error reply is also returned if the underlying
    /// `modulix-core-utils` call fails, in which case the configuration is
    /// left unchanged.
    async fn install_plugin(
        &self,
        module: &str,
        plugin: &str,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.run(
            connection,
            &header,
            ACTION_INSTALL,
            "InstallPlugin",
            &[module, plugin],
        )
        .await
    }

    /// Disables one plugin of a module and rebuilds the system.
    ///
    /// D-Bus signature: `UninstallPlugin(s module, s plugin) -> (s)`.
    ///
    /// # Parameters
    /// * `module` - module key owning the plugin; same namespace resolution
    ///   as `Self::install_plugin`.
    /// * `plugin` - bare plugin name within `module`.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_REMOVE`] polkit
    /// action; the caller's polkit agent may prompt for authentication.
    ///
    /// # Returns
    /// A human-readable status line of the form
    /// `"plugin <plugin> uninstalled for module <module>"`, once the
    /// rebuild has completed.
    ///
    /// # Post-conditions
    /// The call blocks for the whole minutes-long `nixos-rebuild`. Only the
    /// plugin leaves `module`'s plugin list; the module itself stays
    /// enabled. Same dry-run and cache-invalidation behaviour as
    /// `Self::install_plugin`.
    ///
    /// # Errors
    /// Same failure modes as `Self::install_plugin`: denied polkit
    /// authorization, unresolvable `module` metadata/namespace, or the
    /// underlying `modulix-core-utils` call failing (configuration left
    /// unchanged).
    async fn uninstall_plugin(
        &self,
        module: &str,
        plugin: &str,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.run(
            connection,
            &header,
            ACTION_REMOVE,
            "UninstallPlugin",
            &[module, plugin],
        )
        .await
    }

    /// Applies scalar option and/or list-entry changes in a single call.
    ///
    /// Unlike every other method on this interface, `SetOptions` does not
    /// dispatch through `Daemon::run` or the [`Command`] registry at all
    /// — it authorizes the caller itself and then calls
    /// `crate::command::setting::apply_option`/`crate::command::setting::apply_list`
    /// directly. One consequence: it never invalidates the `Store1`
    /// installed-listing cache, since options/lists are not part of that
    /// listing.
    ///
    /// D-Bus signature: `SetOptions(a(ssb) options, a(ssb) lists) -> (s)`.
    ///
    /// # Parameters
    /// * `options` - scalar-option changes to apply, each a `(name, value,
    ///   reset)` triple; applied first, in array order, via the
    ///   scalar-option library call.
    /// * `lists` - list-entry changes to apply, same triple shape; applied
    ///   after `options`, in array order, via the dedicated list-entry
    ///   library call.
    ///
    /// Either array may be empty; pass an empty array for `lists` to only
    /// change options, and vice versa. Each triple is `(name, value,
    /// reset)`:
    /// - `name`: the option/list key.
    /// - `value`: the value to set. Ignored when `reset` is `true`; pass an
    ///   empty string in that case.
    /// - `reset`: if `true`, restore `name` to its default value instead of
    ///   setting it to `value`.
    ///
    /// # Pre-conditions
    /// The caller must be authorized for the [`ACTION_INSTALL`] polkit
    /// action; the caller's polkit agent may prompt for authentication. This
    /// is the single check made for the whole call — it applies uniformly to
    /// every entry in both `options` and `lists`, including `reset` entries,
    /// there being no dedicated "reset" or "remove" polkit action for this
    /// method.
    ///
    /// # Returns
    /// One status line per entry (all of `options` first, in order, then all
    /// of `lists`, in order), joined with `\n` — e.g. `"option <name> set to
    /// <value>"`, `"option <name> reset to default"`, `"list <name> entry
    /// set to <value>"` or `"list <name> reset to default"` per entry.
    ///
    /// # Post-conditions
    /// Each entry is applied by `crate::command::setting::apply_option` or
    /// `crate::command::setting::apply_list`, which — unlike the
    /// `Install*`/`Uninstall*` methods above — gate their actual library
    /// call behind `#[cfg(not(debug_assertions))]` rather than
    /// [`crate::dry_run::is_dry_run`]: in a debug build the call is always
    /// skipped at compile time, regardless of `MX_DAEMON_DRY_RUN`; the
    /// returned status string is produced either way. Options and lists are
    /// applied sequentially and independently; there is no single rebuild
    /// transaction spanning the whole call the way there is for the
    /// `Install*`/`Uninstall*` methods, so this call is not expected to
    /// block for minutes the way those do.
    ///
    /// # Errors
    /// A D-Bus error reply is returned, and nothing is applied, if the
    /// caller declines or is otherwise denied polkit authorization. If
    /// applying one entry fails, the error is returned immediately and any
    /// remaining entries (in the current array and in `lists` if the
    /// failure was in `options`) are not applied.
    ///
    /// # Example (busctl)
    ///
    /// Set option `theme` to `dark`, reset option `font-size` to default,
    /// and add `vim` to list `favorites`:
    ///
    /// ```sh
    /// busctl call org.modulix.Daemon /org/modulix/Daemon org.modulix.Daemon SetOptions \
    ///   "a(ssb)" 2 "theme" "dark" false "font-size" "" true \
    ///   "a(ssb)" 1 "favorites" "vim" false
    /// ```
    async fn set_options(
        &self,
        options: Vec<Setting>,
        lists: Vec<Setting>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        polkit::check(connection, &header, ACTION_INSTALL).await?;

        let mut results = Vec::with_capacity(options.len() + lists.len());

        for option in &options {
            results.push(apply_option(option).await?);
        }

        for list in &lists {
            results.push(apply_list(list).await?);
        }

        Ok(results.join("\n"))
    }
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
