
One Postgres database. Nothing to prepare beyond creating it and a role that
owns it.

```sh
createdb faber
```

**Migrations run automatically when the API starts.** They are compiled into the
binary with `embed_migrations!("migrations")` and applied against `DATABASE_URL`
before the server binds a port. There is no separate migrate step, no
`diesel migration run` in your deploy pipeline, and no image whose only job is
to migrate.

Two consequences worth planning for:

- **Deploy order is "point it at the database and start it."** If you were about
  to add a migration job, don't.
- **Two API replicas starting simultaneously against a fresh database will race
  on migrations.** Faber is designed to run as a single process; if you scale it
  horizontally you must start the first instance alone.

A migration failure is a hard panic before the listener binds, so a bad
migration is a container that will not come up rather than a server serving
wrongly.

---

## 3. The API

### Build

```sh
docker build -f crates/api/Dockerfile -t faber-api .
```

The build does more than compile the server. It produces, in one image:

- `faber-api` — a glibc binary built against `debian:bookworm-slim`.
- `faber-agent-x86_64` — a **statically linked musl** binary, built in a separate
  stage with its own cargo cache, landing in
  `/usr/local/share/faber/agent/faber-agent-x86_64`.

The agent is built here on purpose: the binary the API serves to an enrolling
host and the server that will talk to it are always the same release. It is
static (`crt-static` plus `relocation-model=static`) because agent transport
exists precisely for hosts whose libc Faber does not get to choose — a glibc
build from this image dies on Debian 11 with `GLIBC_2.34 not found`.

The image runs as the unprivileged `faber` user and exposes **3001**.

### Configuration

Everything is environment variables. Required ones panic at boot if absent,
which is deliberate — a Faber that starts with half its configuration is worse
than one that does not start.

#### Required

| Variable | Notes |
|---|---|
| `DATABASE_URL` | `postgres://user:pass@host/faber`. Also used for the migration connection. |
| `FABER_MASTER_KEY` | 32 random bytes, base64. Panics if missing, not valid base64, or not exactly 32 bytes. |
| `SURGE_SERVICE_TOKEN` | Required by the remote auth provider. Only optional under the test provider. |

Generate the master key once, and keep it:

```sh
head -c 32 /dev/urandom | base64
```

**Losing or rotating `FABER_MASTER_KEY` makes every stored credential
undecryptable.** It encrypts users' provider API keys and SSH key material at
rest. There is no re-wrap path — a rotation means every user re-enters every
credential. Treat it like a database encryption key, because it is one.

#### Auth

| Variable | Default | Notes |
|---|---|---|
| `SURGE_URL` | `http://localhost:3000` | The Surge server. |
| `SURGE_COOKIE_DOMAIN` | `.panit.dev` | **Change this.** The default is Panit's own domain. |
| `SURGE_AUTH_UI_ORIGIN` | `http://localhost:3000` | Origin serving the credential-entry UI. |
| `SURGE_SESSION_TTL_SECS` | `259200` (72h) | Must match upstream's. |

The browser-facing perimeter is mounted at **`/api/surge`** and reverse-proxies
to Surge, so the frontend only ever talks to Faber. The frontend's surge-client
`baseUrl` must match that prefix.

#### Networking

| Variable | Default | Notes |
|---|---|---|
| `API_PORT` | `3001` | Set to 3001 in the image already. |
| `CORS_ORIGIN` | *(none)* | Comma-separated. **Must list the UI's origin** or every page load resolves to signed-out. |
| `FABER_PUBLIC_URL` | *(none)* | This API's externally reachable base URL. |

`CORS_ORIGIN` is load-bearing twice: it governs Faber's own routes *and* is
passed through as the session zone's allowed origins. Leave it empty and the
frontend's `whoami` fails CORS, which the client cannot distinguish from an
unreachable auth perimeter — so the symptom is "nobody can log in", not a CORS
error anyone will notice.

`FABER_PUBLIC_URL` is optional at boot and every route works without it —
**except agent enrollment**, which cannot build an install command without
knowing what a machine out there should dial. `surge_url` is the auth service
and `cors_origins` names browsers; neither answers that question. If you skip
it, you get a healthy API and an unservable install command, discovered when you
try to add your first host.

#### Policy

| Variable | Default | Notes |
|---|---|---|
| `FABER_ALLOW_LOCAL_HOSTS` | **`true`** | Whether users may register hosts reached through the API process itself. |

The default is permissive: anything other than the literal string `false` enables
it. On a multi-user deployment **set this to `false`.** A local host is one
executed inside the API container, which means one user's run touching the API's
own filesystem and process namespace. Disabling it leaves existing local hosts
intact; it only refuses new ones. It is surfaced to the frontend at
`GET /api/config`.

#### Search (optional)

| Variable | Notes |
|---|---|
| `SEARXNG_URL` | One named SearXNG instance. Set it and every run gets the `search` tool. |
| `PARALLEL_API_KEY` | Parallel Search API key, used when no SearXNG instance is named. |
| `SEARCH_PUBLIC_NETWORK` | `true` to search the public SearXNG pool. Ignored when `SEARXNG_URL` is set. |
| `SEARCH_PROXY` | Outbound proxy, for search traffic only. |

`SEARCH_PROXY` is passed explicitly rather than read from `HTTPS_PROXY` because
this is a multi-user service and nothing about one user's run may be decided by
the host's ambient environment. The same principle governs Docker endpoints and
host configuration throughout.

#### `SURGE_TEST_PROVIDER` is a security boundary

`SURGE_TEST_PROVIDER=true` authenticates **every request as one fixed
identity**. It needs two locks turned:

1. The `test-provider` Cargo feature at build time.
2. The environment variable at run time.

**A production image must be built without the feature.** The standard
`crates/api/Dockerfile` does not enable it, so the environment variable alone
can do nothing — which is the intended arrangement. If you maintain a custom
build, do not carry the feature into it. The related `SURGE_TEST_USERNAME` and
`SURGE_TEST_DISPLAY_NAME` are development-only and have no production meaning.

Likewise, every `FABER_TEST_*` variable in the codebase configures the test
suite, not a deployment.

### Health

`GET /health` returns `{"ok": true}` with no authentication and no database
access. It answers "the process is up", not "the process is healthy" — it will
report ok while Postgres is unreachable. Use it as a liveness probe, not a
readiness one.

---

## 4. The UI

### Build

```sh
docker build -f packages/faber-ui/Dockerfile -t faber-ui .
```

Note the build context is the **repository root**, not the package directory.

### Configuration

The UI's configuration is **injected at runtime, not baked at build time**.
`server.ts` reads the environment on each request and inlines a
`window.__FABER_RUNTIME_CONFIG__` script into the HTML shell. One image
therefore serves every environment.

| Variable | Default | Notes |
|---|---|---|
| `FABER_API_URL` | `""` | Base URL of the API. Falls back to `API_URL`. Empty means same-origin. |
| `FABER_AUTH_MODE` | *(none)* | `inline` or `redirect`. Falls back to `AUTH_MODE`. |
| `PORT` | `3000` | |
| `HOST` | `0.0.0.0` | |

Two caching behaviours the server implements deliberately, worth knowing before
you put a CDN in front of it:

- `/assets/*` is served `immutable` with a one-year max-age, and a **miss there
  returns a genuine 404** rather than the HTML shell. Hashed assets are
  content-addressed; returning HTML would hand a stale client an opaque MIME
  error instead of a recoverable chunk-load failure.
- The shell itself is `no-store`, because it carries the injected runtime config.

### Wiring the two together

If the UI is on `https://faber.example.com` and the API on
`https://api.faber.example.com`:

```sh
# UI
FABER_API_URL=https://api.faber.example.com

# API
CORS_ORIGIN=https://faber.example.com
FABER_PUBLIC_URL=https://api.faber.example.com
SURGE_COOKIE_DOMAIN=.faber.example.com
```

Same-origin behind one proxy is simpler and avoids the CORS question entirely:
leave `FABER_API_URL` empty and route `/api/*` to the API.

---

## 5. Agents

An agent is a daemon on a machine Faber reaches. It **dials out** — there is no
inbound port to open on the target and no firewall change to make.

Everything that runs on that machine runs through this one connection: exec,
file transfer, and the forwarded Docker socket.

### Enrolling

The owner of a host requests enrollment from the host detail view or
`POST /api/hosts/{id}/agent/enroll`, which returns a copy-pasteable command
carrying a single-use token good for one hour.

For a machine a **user** owns:

```sh
curl -fsSL https://api.faber.example.com/api/agent/install.sh | sh -s -- --token <token>
```

For a machine running a system agent:

```sh
curl -fsSL https://api.faber.example.com/api/agent/install.sh \
  | sudo sh -s -- --system --token <token>
```

The difference is not cosmetic. A user install writes a `systemctl --user` unit
under that account's authority and keeps its identity in
`$XDG_CONFIG_HOME/faber-agent`. A system install writes
`/etc/systemd/system/faber-agent.service`, `/usr/local/bin/faber-agent`, and
`/etc/faber-agent/config.json` (mode `0600`, holding the daemon's SSH host key
and connection credential).

**Privilege is fixed at install and there is no protocol for changing it.**

### Credentials

The agent's credential is equivalent to root on its machine: whoever holds it
can displace the running daemon and receive that host's launches. Treat it
accordingly.

Re-enrolling a host with a fresh token replaces the credential. That drops the connection and
leaves every tenant, reservation, and directory untouched — reinstalling with a
fresh token restores service.

The config file *is* the identity. There is no server-side copy to reconcile
against; losing the file means reinstalling, not resyncing.

### The agent binary is x86_64 only

`GET /api/agent/binary/{arch}` serves one file per architecture from
`FABER_AGENT_BINARY_DIR`, named for what `uname -m` reports on the target. The
standard Dockerfile builds **only `faber-agent-x86_64`**.

**An arm64 machine cannot enroll today.** The installer will fail to fetch a
binary for its architecture. If you need one, add an `aarch64-unknown-linux-musl`
stage to `crates/api/Dockerfile` mirroring the existing musl stage and copy the
result in beside the x86_64 file.

In a development checkout, `FABER_AGENT_BINARY_DIR` defaults to `target/release`
and the server will serve a locally built binary for its own architecture, so
`cargo build --release -p faber-agent` is enough to make enrollment work under
`cargo run`.

---

## 6. Operating

### Deploy order

1. Postgres reachable.
2. Start the API. Migrations apply automatically; a failure is a container that
   will not come up.
3. Start the UI.
4. Create a host owned by the account that will use it.
5. Enroll its agent from the host detail view or `POST
   /api/hosts/{id}/agent/enroll`.

### Upgrading a deployment

The service-host removal migration is destructive. **Take and verify a full
Postgres backup before deploying it.** It deletes shared hosts and images,
their dependent rows, tenancy grants, and subject ids because those records have
no owner to migrate them to. The down migration restores schema shape only; it
does not restore deleted data.

Deploy the migration and the owned-only API together. Do not roll back only the
application binary after the migration has run: the removed endpoints and
schema are no longer compatible with the old service-host implementation.


The API applies pending migrations at boot, so an upgrade is a normal image
roll — with the single-process caveat from §2 about concurrent starts.

Because the agent binary is built into the API image and served from it, an API
upgrade changes the binary new hosts receive. **Existing agents are not
upgraded**; they keep running the binary they installed. Re-running the install
command with a fresh enrollment token is how an agent is updated.

### What to back up

- **Postgres.** Everything durable is here.
- **`FABER_MASTER_KEY`.** Not in the database, and its loss is unrecoverable in
  the sense that matters — see §3.
- **Each host's `/srv/faber`** (or whatever `user_data_root` is), if
  tenant workspaces are worth keeping. Faber has no backup mechanism and no
  export path.

Agent config files (`/etc/faber-agent/config.json`) are worth backing up only if
re-enrolling is inconvenient; re-running the installer with a fresh token is the
supported recovery.

### Logs

The API logs through `tracing` to stdout with an HTTP trace layer. There is no
log file and no rotation to configure — that is your runtime's job.

`RUST_LOG` sets the filter. **The default when it is unset is
`api=debug,tower_http=debug`**, which is a development default and is noisy in
production. Set it explicitly:

```sh
RUST_LOG=api=info,tower_http=warn
```

### `.env` files

The API calls `dotenvy::dotenv()` before reading anything, so a `.env` file in
the working directory is loaded if present. That is convenient in a checkout and
a hazard in an image — a stray `.env` baked into a container silently overrides
nothing (real environment variables win) but a *missing* one can make a locally
working configuration look like a deployment bug. Prefer real environment
variables in production and treat `.env` as a development affordance.

---

## 8. Things that are not deployed

Worth stating so you don't go looking:

- **`crates/traefik`** is a library for expressing domain → container routing as
  Traefik dynamic configuration. **It has no caller.** Nothing in the API
  references it, so there is no Traefik integration to configure today.
- **`crates/search`, `crates/readable`, `crates/llm`, `crates/harness`,
  `crates/environment`** are libraries linked into the API, not separate
  services.
- There is **no message queue, no cache, no object store.** Postgres and the
  filesystem are the whole of the state.
- There is **no metrics endpoint.** `/health` is liveness only.
