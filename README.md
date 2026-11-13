# herdr-devtunnel

Connect Herdr cores through an owner-only private Dev Tunnel instead of SSH. The server exposes an `hc` session through a loopback bridge; the client registers that bridge as a machine in its local core. Herdr App can then use that core to access the remote machine and its spaces.

## Prerequisites

Install the executable on both machines from the [GitHub repository](https://github.com/Syndim/herdr-devtunnel) with Rust 1.89 or later:

```sh
cargo install --locked --git https://github.com/Syndim/herdr-devtunnel.git
hd herdr --help
```

Also install Herdr Core (`hc`) and the official Microsoft Dev Tunnels CLI (`devtunnel`) on both machines. The client needs an `hc` that supports `machine add --via`; the server needs `hc connect --session`. On Unix, `hc` runs `--via` through `sh`, which must be on `PATH`. On Windows, Herdr Core 0.1.20 and later launch `--via` directly, so this tunnel does not require Git or `sh` on the Windows client. This behavior is documented for [Herdr Core 0.1.21](https://github.com/herdrdev/herdr-core/blob/v0.1.21/docs/machines.md#use-another-way-in). Other Herdr Core features may have separate shell requirements. Herdr App is optional on the client for the desktop UI.

The client emits POSIX shell quoting on Unix and Windows command-line quoting on Windows. For client roles, `doctor` checks for `--via` in `hc` help and, on Unix, `sh` on `PATH`. On Windows, both `doctor` and client registration require the help output to advertise `CommandLineToArgvW` and `no shell`; this check does not prove runtime compatibility.

Sign in to Dev Tunnels with the **same account and identity provider** on both machines. Check prerequisites and optionally start interactive browser login with:

```sh
hd herdr setup --role server --login
hd herdr setup --role client --login
hd herdr doctor --role both --json
```

Run the server and client setup commands on their respective machines. `--role` accepts `server`, `client`, or `both` (the default). Without `--login`, setup checks dependencies, `hc` capabilities, and login, then prints next steps; it does not install dependencies, create tunnels, or register machines. `--login` and `--json` cannot be combined. Use `--hc PATH` and `--devtunnel PATH` to select executables explicitly.

## Server

Run the server as the ordinary user who owns the projects and agent credentials:

```sh
hd herdr server --tunnel agent-host
```

Wait for successful startup and the `CLIENT COMMAND` line; use the full tunnel identifier printed there on the client. A bare name uses automatic relay selection; a name with a cluster suffix, such as `agent-host.inc1`, requests that relay when creating the tunnel. Existing tunnels are not relocated, and a mismatched relay is rejected.

The server looks up the named tunnel and creates it only on a confirmed not-found response. It reuses an eligible private tunnel and adds the port if it is empty; incompatible ports or access grants are rejected. The default bridge port is `43821` and the remote `hc` session is `default`. Override them with `--port 43822 --session work`, and use the same values on the client. Use a dedicated tunnel, port, and server process for each published session.

By default the server is a managed background process. `serve` is an alias for `server`. Manage profiles independently:

```sh
hd herdr status
hd herdr stop
hd herdr server --name workbox --tunnel agent-host
hd herdr status --name workbox
hd herdr stop --name workbox
```

The default profile is `default`. Starting a compatible running profile leaves it unchanged; stop it before changing settings. State, configuration, and `server.log` belong under `~/.herdr-devtunnel/server-PROFILE`. On Unix, `HOME` must be owned by the current user and must not be group or world writable; its ancestors are checked for trusted ownership and permissions, and symlinks along the path for trusted ownership. The runtime directory must be owned by the current user and have mode `0700`; new profile directories are created with mode `0700` subject to the umask, but existing profile directory permissions are not tightened or checked for `0700`. On Windows the runtime directory is under `%USERPROFILE%\.herdr-devtunnel`; the program neither enforces nor validates private Windows ACLs. Restrict the directory's inherited and explicit ACLs, as well as those on its contents, to the intended user and necessary trusted system principals, especially on shared machines. A managed server copies the running executable to a snapshot in this directory, so stop and start the profile to load updated code. Do not modify the source executable in place while starting or reconnecting: the snapshot copy is not atomic with such updates. No service or login autostart is configured.

The guard retries the Dev Tunnels host after exits, known failures, or readiness timeouts, rechecking tunnel policy before retrying. It does not restart `hc` or agents. `--cooldown 2` sets the delay in seconds; `--max-restarts 0` means unlimited retries; repeatable `--pattern REGEX` adds case-insensitive failure patterns. `--timeout` bounds setup calls and host readiness. The managed startup readiness timer is `8 * --timeout + 60` seconds and starts after the worker is spawned; earlier work, including copying the executable snapshot, is not covered by it, so it is not a bound on total startup time. If managed startup times out, it requests shutdown but does not wait to confirm that the worker has stopped; check `status` and the log before retrying. Cleanup after an interrupted startup has not been verified. Server `status` reports whether the managed worker is running, its readiness and restart count, and the last status when stopped; it does not establish client connectivity or agent health.

For attached troubleshooting, use `hd herdr server --foreground --tunnel agent-host` and stop it with Ctrl+C. Foreground servers are not controlled by `status` or `stop`. Stopping a server closes bridge connections and the tunnel host, but does not intentionally stop the persistent `hc` core or agent terminals and does not delete the tunnel.

## Client

Once the server is ready, run on the client, substituting the identifier and settings printed by the server:

```sh
hd herdr client --name workbox --tunnel agent-host.inc1
hc machine ls
hc exec --on workbox whoami
```

`add` is an alias for `client`. Client `--session` selects the **remote** session (default `default`); it must match the server's `--session`. Client `--local-session` instead selects the local core for registration: only for `hc machine add`, it sets `HC_SESSION` to the supplied value and removes inherited `HC_SOCKET`. It does not alter the remote session, the saved transport, or the environment of subsequent commands. Without `--local-session`, `hc machine add` inherits both variables unchanged. A socket configured in `hc` may still override session selection; check its configuration if the wrong core is used. Use Herdr App and subsequent `hc` commands with the same local core; do not use the App's SSH setup, SSH Login, or Update Core actions for this connection. Maintain the remote core directly. Do not connect a core to itself or create connection cycles.

For example, with different local and remote sessions (POSIX shell; replace the example tunnel identifier with the full one printed by the server):

```sh
# On the server:
hd herdr server --tunnel example-tunnel --session remote-work

# On the client, assuming the server printed example-tunnel.inc1:
hd herdr client --name remote-box --tunnel example-tunnel.inc1 --session remote-work --local-session local-desktop

# Select the same local core for subsequent hc commands:
(
  unset HC_SOCKET
  export HC_SESSION=local-desktop
  hc machine ls
)
hd herdr reconnect --all --role client --local-session local-desktop
```

Registration pins the executable paths of `hd` and `devtunnel`; keep them stable. If either executable is renamed or relocated, check the registrations in the same local core used for registration, remove only the affected machine, then register it again. For the `local-desktop` example above, select that core and inspect the list (POSIX shell):

```sh
(unset HC_SOCKET; export HC_SESSION=local-desktop; hc machine ls)
```

A socket configured in `hc` may override this selection; verify the core and its configuration before removing anything. Once you have confirmed `remote-box` is the affected registration in that core, remove it with the same selection:

```sh
(unset HC_SOCKET; export HC_SESSION=local-desktop; hc machine rm remote-box)
```

In particular, reconnect cannot repair a registration whose executable has been renamed or relocated and whose pinned path no longer exists. The client command does not overwrite or remove registrations. Do not repeatedly re-register a machine merely because it is reconnecting.

## Agent status

Agent status integration is optional and is not required for the tunnel. `hd herdr copilot-status EVENT` reports Copilot lifecycle events to Herdr's Agents panel when invoked by a configured hook. Outside a Herdr terminal it does nothing; inside one it reads a JSON object from stdin and writes the status to the controlling terminal. `permissionRequest` and `agentStop` do not report completion or a human prompt. A root `sessionIdle` event reports completion, or idle if aborted; without a separately configured hook or extension that supplies this root event, completion is not reported by this integration. Hooks and extensions are not installed or configured by this project; use `--help` for supported events.

## Reconnect

```sh
hd herdr reconnect --all
hd herdr reconnect --all --role server
hd herdr reconnect --all --role client --session work
```

`r` is an alias for `reconnect`, and `--all` is required. With the default `--role both`, it restarts all **running managed** server profiles with their saved settings, waits for readiness, then asks the local core to reconnect recognized tunnel machines. It leaves stopped profiles, foreground servers, SSH machines, and other transports alone. Server restarts close bridge connections; downtime can be prolonged, and readiness may not return if restart fails. They do not restart persistent cores or agents. For each recognized registration, reconnect calls `machine.connect` without first disconnecting it; it does not remove or re-register machines. Reconnect `--session` and its alias `--local-session` select the local core session containing registrations, not the remote session; both default to `default` when omitted and do not filter server profiles. `--hc PATH` selects the local executable and `--timeout` bounds each client API call. Failures are reported per target, remaining targets are attempted, and any failure yields an unsuccessful exit. Reconnect does not perform login.

Reconnect lists machines through the local `hc` API and recognizes only `--via` registrations whose standalone `herdr connect` command has the expected options and an absolute executable path resolving to the same canonical path as the running executable, including through symlinks. Missing paths and different paths, even identical copies, do not match. Replacing the binary at the same path preserves recognition; relocating it can require re-registration. Each API operation opens a separate `hc connect` process; a machine is reported connected only when `machine.connect` returns state `connected`.

The server bridge and reconnect clear inherited `HC_SOCKET` when starting their respective `hc connect --session` processes. They do not clear `HC_SESSION`, and an `hc`-configured socket may still override `--session`; check the `hc` environment and configuration if the wrong core is selected. Server and reconnect `--session` default to `default` rather than reading `HC_SESSION`. Client registration without `--local-session` leaves both environment variables inherited.

## Security and protocol

The server checks for a dedicated owner-only private tunnel with no additional tunnel or port access grants. An existing tunnel may have no ports (the server adds the requested port) or exactly one matching port. In addition to inspecting tunnel details, the server explicitly runs `devtunnel port list` even if the details omit ports, and `devtunnel access list` for the tunnel and, when the port exists, for that port. Unexpected ports, nonempty ACLs, invalid JSON, failed checks, and unrecognized formats for the fields the checks require prevent hosting. The checks do not reject every unrecognized field: extra fields in tunnel details, port entries, ACL objects, and access-list responses can be ignored. Do not assume they detect policy represented by an unknown field. Checks run before hosting, after adding a port, and before host retries, not continuously. They cannot detect already-issued access tokens; an empty access policy does not prove that no such tokens exist. Never enable anonymous access or change ACLs after startup.

The server bridge binds IPv4 loopback, but local processes can reach it without Dev Tunnels authentication. While the client is connected, `devtunnel connect` also exposes a loopback forwarding port on the client; local processes there can use it to reach the server bridge through the authenticated tunnel. The protocol/session handshake authenticates neither endpoint and is **not a password**. A local process that can reach either endpoint and send the expected handshake can gain `hc` access, effectively granting shell and file access as the server user. Use trusted single-user machines or appropriate OS isolation on both ends.

The connector validates the expected connection-only `hc` bootstrap format and never executes the supplied shell line. Unknown formats fail closed. It exchanges a bounded `herdr-devtunnel/1` JSON handshake, then forwards binary `hc` traffic; each connection starts `hc connect --session`, not an arbitrary remote shell. After client input closes, the bridge allows up to 10 seconds for a final response before cleaning up that connection. OS logout, reboot, and host policies may still terminate user sessions.

## Troubleshooting and testing

- Run `doctor` for the relevant role; check executable availability, `hc` capabilities, and login on both machines, plus the client shell requirement described above. Authentication or network errors must not be treated as permission to create a replacement tunnel.
- If startup fails or readiness is lost, inspect `status` and the profile's `server.log`. Check the port is free, the tunnel identifier and relay are correct, and its ACL and port meet the private-tunnel policy. Stop conflicting foreground processes only after verifying their identity.
- If a client cannot connect, check that the server is ready, both ends use the same tunnel, port, and remote session, and both are signed in with the same account and provider. Verify the pinned executable paths and supported `hc` bootstrap format.
- Treat doctor output, terminal output, and `server.log` as sensitive: they can include account identifiers, local paths, tunnel and session details, and output from `hc` or `devtunnel`, potentially including credentials or tokens. Restrict access and review and redact them before sharing; do not assume secrets are automatically removed. Avoid making changes through the remote connection that the change will close.

Build with `cargo build`. To run fixture tests without network access, first cache the locked dependencies (for example, run `cargo fetch --locked` while online), then run `cargo test --locked --offline`. The tests need `rustc` on `PATH` to compile fixture executables, a writable and executable temporary directory, subprocess execution, and IPv4 loopback access. On macOS and Linux, `sh` and `kill` must be on `PATH`; on Windows, `tasklist` is needed, but Git and `sh` are not required. Fixtures replace `hc` and `devtunnel`; no real tunnel or login is needed. A plain `cargo test` may download dependencies.

The integration targets Windows, macOS, and Linux, but builds and fixture tests do not establish real cross-machine compatibility. Before relying on it, validate actual login, tunnel hosting and connection, platform-specific `--via` invocation, App visibility, interactive agent approvals, Unicode, reconnect, and session persistence on the deployment machines.

## License

MIT; see [LICENSE](LICENSE). Include the copyright and permission notice with copies or substantial portions of the software.
