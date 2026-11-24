# wsp multiplayer: a small hub and a local wsp per person

Nov 24, 2026 · @Jesper · Hub and spoke, as built

Two people, Jesper and Ed, work on one project with their own agents, each on their own machine, and see and hand each other's work. This document is how that works. The design of record is the Claude Doc of this name; the decisions are the wsp project's d5, d6 and d7 (`wsp project show wsp --decisions`). The earlier design, one wsp server for an org of groups, is retired: it is under "Later", and the old text is in git (`git show fdade73:docs/multiplayer.md`).

## Purpose

The collaboration this serves is the one between two people who each already run wsp:

- **Each keeps their own wsp.** Daemon, manager, seats, worktrees, checks and builds run on the person's machine, as today. A wsp with no hub works exactly as it always did.
- **They share what they choose to.** A row or a worklist that both should see, take and drive lives in a shared workspace. Everything else stays on its owner's machine and is never sent.
- **One place agrees who holds what.** Who has a row, who drives a list, who is allocated what, and where a message goes are decided by one small hub, not by two machines guessing.
- **Joining, leaving and offline are normal.** Nothing here makes a wsp depend on the hub: a person works alone, joins, goes offline mid-row and syncs back.

The hub hosts no agents and runs no builds (d5). Code moves between the people through their shared git remote, never through the hub.

## Hub and spokes

```
 Jesper's machine                         Ed's machine
 ┌──────────────────┐                    ┌──────────────────┐
 │ wsp (a spoke)    │                    │ wsp (a spoke)    │
 │ daemon · manager │                    │ daemon · manager │
 │ seats · worktrees│                    │ seats · worktrees│
 │ private rows     │                    │ private rows     │
 │ synced cache     │                    │ synced cache     │
 └────────┬─────────┘                    └─────────┬────────┘
          │  TLS, a per-user token                 │
          └──────────────►  wsp-hub  ◄─────────────┘
                     objects · refs · leases
                     users · tokens · message queues
```

**The hub** is `crates/hub` (`wsp-hub serve --config <file>`, wsp-460), a separate binary of its own. It holds four things in one data directory (`crates/hub/src/lib.rs:1`):

- `objects/`: every record and event, each in a file named by its hash.
- Refs: per shared workspace, which object is each subject's head.
- Leases, with their fencing tokens, in `hub.db` through `wsp-store-sqlite`.
- Users, tokens and each user's message queue, in the same database.

It links `wsp-api`, `wsp-objects`, `wsp-server`, `wsp-store-contract` and `wsp-store-sqlite` and nothing of the root crate, manager, trees or host (`tests/layering.rs`, and `crates/hub/tests/store.rs`'s `cargo tree` test). It answers on `wsp-server`'s carrier and connection loop, so the hello, request and subscribe frames are the daemon's own.

**A spoke** is a person's ordinary wsp, once it is logged in (`wsp login`, below). Logging in adds one thing: the daemon starts the sync worker, a thread named `hub-sync` (`src/hub_sync.rs:162`, started by `start` at `src/hub_sync.rs:182`) that runs only while `~/.config/wsp/hubs.json` exists. Logging out removes the file and the worker stops at its next turn. Without a login no hub code runs and nothing is written.

**The worker's turn** runs every 10 seconds (`TURN`, `src/hub_sync.rs:165`; `$WSP_HUB_SYNC_MS` changes it) and at once when a task file changes. It takes each task of a shared workspace, makes a record of it if its file moved since the synced head, journals that record, and syncs through `wsp_client::Client::sync` (`crates/client/src/sync.rs:332`). A hub that does not answer is a state, not a failure: one line in `cycle.log` when it changes, then another try after a wait that doubles from 1 s to 60 s (`BACKOFF_MIN`, `BACKOFF_MAX`, `src/hub_sync.rs:171`).

**The wire** is `wsp-api`: the hub's verbs are `sync.heads`, `sync.have`, `sync.put`, `sync.get`, `refs.move`, `ids.alloc`, `lease.take`, `lease.renew`, `lease.release`, `msg.send`, `msg.take`, `msg.ack`, and the identity commands `hub.user.add`, `hub.user.revoke`, `hub.token.issue`, `hub.token.revoke`, `hub.workspace.add` and `hub.member.set` (`crates/api/src/lib.rs:133`). The hub's port is 7489, the one after the daemon's own API (`DEFAULT_PORT`, `crates/hub/src/config.rs`). The daemon's server still admits loopback and the tailnet only; the hub's peer list is its own.

## Hash sync

The hub is content-addressed and synced by comparing hashes, git-like (d6). The format is `crates/objects/src/lib.rs`.

- **An object is immutable.** One is a version of an entity (`Object::Record`, `crates/objects/src/lib.rs:135`: a row, a list, a seat, whole, with its `parent` version) or one thing that happened (`Object::Event`). Its `ObjectId` (`:56`) is the SHA-256 of its canonical bytes (sorted keys, no whitespace; `encode` at `:195`, `verify` at `:222`), so there is no update: a change is a new object whose parent is the old one. An id names one sequence of bytes, so an object can be cached anywhere and is stored once however many spokes send it.
- **Refs are the only thing that moves.** `Refs` (`:284`) say, per workspace, which object is each subject's head. Two sides sync by comparing refs (`diff`, `:370`) and sending only the objects behind the subjects that differ.
- **A move is a compare-and-swap.** `refs.move` names the head it expects. Of two moves from one head exactly one is written, in one `BEGIN IMMEDIATE` transaction; the other is answered `ok` with the hub's head and the word `stale` (`crates/hub/src/lib.rs`, `Hub::move_ref`). It is never retried over the hub.
- **A stale edit is merged, not lost.** The spoke takes the hub's head through the door and merges three versions: the common ancestor (found through parents), its own file and the hub's head (`src/reconcile.rs`). A field changed on one side takes that side; the `## Log` and `## Decisions` sections are the union of both sides' dated entries; a field changed on both keeps the hub's and writes the spoke's value into the log as a conflict note, tagging the row for its user. The merge is pushed as a new object whose parent is the hub's (`src/hub_sync.rs:148`, `after_stale`).
- **Ids are the hub's.** `ids.alloc` hands out a row's id from one counter per project, never reused, so two spokes cannot make the same id (`crates/hub/src/lib.rs`, "Ids").
- **The synced cache** (`src/hub_cache.rs`, wsp-465). A spoke keeps its own rows whole, as files the import door writes, so `ls`, `show`, `wip` and `worklist show` read them like any local row. A row is its own when it was made here, is leased here, or is allocated to its user. Every other row is a head: title, status, holder and allocation in `$WSP_STATE/hub/heads.json`, shown in `ls` with no body, and never pushed back. `wsp show <id> --fetch` makes one whole, as does leasing or allocating it (`promote`, `src/hub_cache.rs:226`). The worker follows the hub's event feed and keeps its position in `stream.json`; one that starts further than 100 events behind (`GAP`, `:42`) resumes from the head and lets the heads diff stand in for what it skipped.

## Private and shared workspaces, and publish

A workspace is a sync scope (d7).

| | Private | Shared |
| --- | --- | --- |
| Bound to | The local wsp | The hub |
| Who sees it | Its machine alone: its rows, lists, seats, asks and events never leave | Its members |
| Leases, allocation, routing | None; today's wsp | All of them apply |
| Named | `private` | `shared:<name>` |

- **New work is private.** `wsp add`, `wsp worklist new` and `wsp project add` file into the caller's workspace, private unless `--workspace shared:<name>` or `$WSP_WORKSPACE` says otherwise, the flag first, and a shared one needs a login. A child (sub-task, leg, piece) takes its parent's workspace (`src/record_workspace.rs:19`).
- **Moving work across is `wsp publish`.** `wsp publish <row|list> --to shared:<name>` (`src/cmd_publish.rs:195`) moves a private row, or a list with its member rows and their sub-tasks and pieces, into a shared workspace. The hub is asked first and a refusal changes nothing: each row takes a new id from `ids.alloc`, then is renamed and stamped `workspace:` (the old id keeps resolving through `ids.json`), and the sync worker pushes it on its next turn. A claimed row is published under a hub lease, so a refusal leaves every row as it was.
- **Refused:** a logged-out spoke, a row already shared, a sub-task or piece on its own, and a workspace the hub says the caller is not a member of. There is no unpublish in v1.
- **The hub scopes everything to membership** (`crates/hub/src/scope.rs`). A user reads and writes a shared workspace they are a member of and nothing else; heads, moves and ids outside it are `forbidden`; an object is served only if a member workspace references it, so knowing a hash is no right to its bytes. An admin is no exception: an admin who is not a member reads none of the content. `crates/hub/tests/scope.rs` and the privacy gate (wsp-180, `tests/privacy.rs`) show a private row, list, seat and ask reaching no frame and no other user.
- **Workspaces and members** are made from an admin's spoke: `wsp hub workspace add <name>` and `wsp hub member add|rm <ws> <user>` (`src/cmd_hub.rs`).

Open: whether shared work can be taken back to private (v1 says no), and whether new work should stay private by default (it does, as proposed in d7).

## Leases and offline work

A row in a shared workspace is moved by one user's seat at a time, and the hub says which.

- **Taking.** `lease.take` before `wsp claim` (so before a spawn) and before `wsp start` answers a fencing token and an expiry, or refuses `held`, naming the user whose spoke has it, or `forbidden` for a row allocated to someone else (`src/hub_lease.rs:112`; the hub's side is `Hub::take_row_lease`, `crates/hub/src/leases.rs:178`). The token and the hub's expiry are written into the local claim, so the local expiry pass and the hub agree.
- **Length.** A lease runs 7 days from its take or last renewal (`lease_days` in the hub's config, default 7, at least 1).
- **Renewing.** While the holder seat is alive and the claim has been as it is for an hour, the daemon sends `lease.renew` and moves `expires` to the hub's answer (`renew_due`, `src/hub_lease.rs:198`). Only the hub's word extends a claim that has a hub token.
- **Expiry.** The hub releases a lease past its TTL and announces `lease.expired` (`expire_leases`, `crates/hub/src/leases.rs:266`). The spoke that held it lets go the way `wsp despawn --lost` does, the checkpoint kept in the tree (`follow_expired`, `src/hub_lease.rs:467`).
- **Fencing.** A shared row's verdict and its landing go to the hub as `row.reviewed` and `row.landed` event objects carrying the claim's token (`push_verdict`, `src/hub_lease.rs:257`). The hub refuses `fenced` one under a token that is no longer the row's current one; the spoke then lets go of the claim, and the row stays as its new holder left it.
- **Allocation.** `wsp allocate <row|list> <user>|none` (`src/cmd_allocate.rs:61`) writes a `for:` naming the user whose agents may take the work, on any spoke that user is logged in on. A row's own `for:` wins, else its list's (`allocated_to`, `:22`). A shared row allocated to another user is refused at start, claim and spawn on this spoke before the hub is asked (`allocated_elsewhere`, `:31`), and the run skips it as running elsewhere. A private record cannot be allocated.

**Offline.** A spoke whose hub is unreachable keeps working on the rows it holds.

- Every object it makes for a shared workspace is kept in order in `$WSP_STATE/hub/journal/` until the hub has it (`src/hub_journal.rs`); a claim whose lease is in date is not touched while the hub is away.
- On reconnect each row is settled by the lease it was made under. Renewed under the same token, the row fast-forwards. Refused, the claim is released and the lease taken again: granted, the row is pushed. Held by another user meanwhile, the objects go to the hub as a **side chain** (put, no ref moved), so this spoke's verdict and landing never reach the hub; its code is still on its git branch, which nothing here touches.
- Both users are told. The old holder's spoke records the side chain locally (`$WSP_STATE/hub/side.json`, a line in `cycle.log`) and sends a notice to the hub, which the new holder's spoke takes (`Side`, `notice`, `src/hub_journal.rs:191`, `:370`).
- A push a crash cut short is replayed whole, with no object twice (`Journal::replay`, `:166`): a put is idempotent by hash.

The lease is the bound on offline work: a person can go as far as their leases last and no further without risking the side chain.

## List driving

A worklist is driven by the spoke it is allocated to (d5), and only one spoke at a time advances it.

- **The drive lease.** A shared list is advanced only by the spoke holding the hub lease `drive:<list>` (`drive_row`, `src/hub_lease.rs:478`; the decision is `drive`, `:588`). The daemon renews it while it ticks (`renew_drives`, `:704`); a list no longer running here, done or gone or made private, lets its lease go.
- **Members run where they are held.** The driver leases each member row before spawning it. A member another user's spoke holds runs on that spoke, and that spoke, which does not drive the list, opens the verifier for the member it holds and touches nothing else. `driven_here` (`src/cycle.rs:956`) says in `cycle.log`, once per change, who drives a list.
- **A spoke without the drive lease never advances the list.** It sees the list and its members through the cache and can read who drives it; the barrier, verification and closing steps run on the driver alone.
- **Private lists** are driven by their own wsp as always.

## Login and tokens

Each person has their own token and the hub derives who they are from it, never from what a client says about itself (`crates/hub/src/auth.rs`).

- **Resolving a hello.** A token whose hash is an active token record of an active user names `person:<user>`. The hub's bootstrap token (`api.token` in its data directory, made on first start at 0600) names the hub itself while the hub has no active admin, and may do one thing: add the first admin. Anything else is refused `unauthorised`, saying no more; an unknown token and a revoked one read alike. A request whose `by` is another principal is `forbidden`, as in the daemon.
- **The first admin.** `wsp-hub admin init --config <file> --admin <name>` makes it on the hub's host and prints its token once (`crates/hub/src/admin.rs`). The hub keeps the hash and nothing else; the token is written nowhere.
- **More users.** From a logged-in admin's spoke: `wsp hub user add <name> [--admin]`, `user ls`, `user revoke <name>`, and `wsp hub token <name>`, which issues a new token and revokes the old ones. The token is minted on the spoke and only its hash is sent; it is printed once and is in no log, command, event or file (`src/cmd_hub.rs`).
- **Logging in.** `wsp login <hub-url>` (`src/cmd_login.rs:231`) reads the token from the terminal prompt with echo off, or from stdin, never from the command line. It dials the hub, says hello and, on `welcome`, writes `~/.config/wsp/hubs.json` (`url`, `token`, `user`, `at`, and `ca` for a private CA) at mode 0600; a file that already exists wider is refused, naming it. `user` is the name the hub's `welcome` gave the token. A wrong token, an unreachable hub or a refused certificate writes nothing. One hub per wsp in v1; a second login replaces the first and says so. `wsp logout` removes the file; `wsp hub status` prints the URL and user, never the token; `wsp whoami` asks the hub which principal the token names (`src/cmd_whoami.rs`), and a spoke with no login dials nothing.
- **From the environment.** `$WSP_HUB_URL`, `$WSP_HUB_USER` and `$WSP_HUB_TOKEN` (or `$WSP_TOKEN`) replace the file, for tests and machines (`crates/client/src/credential.rs`).
- **A token never leaves its dial.** `scrub` takes it out of anything a dial said (`src/cmd_login.rs:66`), and the only child process login starts is `stty` with fixed arguments.

A connection keeps the principal it was welcomed as until it closes; a revoked token is refused at the next hello.

## Routing and the user primary

`wsp tell` reaches a row, a seat or an address (`src/address.rs`). The hub carries it past the one machine.

- **Addresses.** `primary` is this machine's own primary seat. `primary:<user>` is that user's primary: on a spoke logged in as them, the local `primary` seat; for anyone else, a remote the hub routes (`src/address.rs:67`, `parse`; `:187`). `seat:<row>` and `governor:<list>` are routed by who holds the row's lease or the list's drive lease when the holder is another user's spoke.
- **How the hub resolves one** (`crates/hub/src/messages.rs`). `seat:<row>` goes to the holder of the row's lease (`row:<row>`), unexpired; `governor:<list>` to the holder of `drive:<list>`; `primary:<user>` to that user, live on the hub. The message is queued for the user, and the first of their spokes to `msg.take` and `msg.ack` it has it. The sender must be a member of the workspace the lease was taken in, or share one with the user whose primary it names: knowing a row's id is no right to reach its holder. A reply (a message whose `re` names one the hub delivered to the sender) goes back to that sender whatever `to` says. What nothing resolves is refused `not_found`, naming the address.
- **The queue** is a revisioned text per user (`hub-inbox:<user>`), each change a compare-and-swap. `msg.take` changes nothing, so a spoke cut off before its ack is handed the same messages again; `msg.ack` removes them. A queued message is announced on the event feed so the recipient's spoke hears it has mail.
- **Locally**, `primary:<user>` for the user logged in here is spooled and ordered like a governor's under its slot; for another user it is one message through the hub, once per level (`route_to_primary`, `src/wake.rs:691`, called from `:253`). A tell with no login is refused, naming `wsp login`.
- **Escalation.** A question escalated past `primary:<user>` is that user's: Needs you shows it `for <user>`, so it says whose it is (`for_user`, `src/attention.rs:378`, used at `:365`).

## Hosting with TLS (wsp-461)

The hub (`wsp-hub serve --config <file>`) serves TLS when its config names a certificate, and a hub on a public endpoint must: it refuses to start bound to an address that is not loopback without one (`Config::tls_required`, `crates/hub/src/config.rs:127`). No host is written in the code or the tests; the hub is whatever the config says, and a tailnet stays optional.

```json
{
  "data_dir": "/var/lib/wsp-hub",
  "bind": ["0.0.0.0"],
  "port": 7489,
  "peers": ["0.0.0.0/0", "::/0"],
  "tls": { "cert": "/etc/wsp-hub/fullchain.pem", "key": "/etc/wsp-hub/privkey.pem" }
}
```

- `tls.cert` is the PEM certificate chain and `tls.key` its private key (relative paths are read from the config's directory). With `tls` every connection is TLS, loopback included (`crates/server/src/tls.rs:18`, rustls with `ring`).
- `peers` is the hub's own list of CIDR blocks it admits; a public hub names `0.0.0.0/0` and `::/0`. It is the hub's alone: the daemon's server still admits loopback and the tailnet and nothing else, and sets no TLS.
- Loopback is always bound; every `bind` address must be in `peers`. A key the file does not know is an error, so a misspelt `peers` does not quietly leave the default.

**The URL.** A client names a hub by `tls://host:port` for TLS, which is the form for any hub that is not on loopback or a private network. `tcp://host:port`, or a bare `host:port`, is plain TCP, as before: the user's token crosses it in the clear. Any other scheme (`https://` too: the protocol is not HTTP) is refused, and a port is required.

**Trust.** A `tls://` hub's certificate must be for the URL's host and chain to the platform's root certificates (`crates/client/src/tls.rs:34`). A private CA (a test, a self-hosted hub) is *pinned* with a PEM file, named by `--ca <file>`, `$WSP_HUB_CA`, or `"ca"` in `~/.config/wsp/hubs.json`: the client then trusts that CA's certificates and no other root. A certificate it refuses ends the dial as `Silent::Tls`, naming the fault (`invalid peer certificate: UnknownIssuer`, `... not valid for name ...`); the user's token is not sent.

**Deploying it.** A container image, a compose file and the step-by-step recipe (certificate, firewall port, backup, upgrade, first login) are wsp-489's: the recipe lives under `docs/` and `deploy/hub/` once that row lands. Hosting and network are configuration, and no machine name or address is a literal anywhere in code, docs or plans (d5).

## Doctor

**What `wsp doctor` says of the hub** (wsp-194; `health`, `src/hub_sync.rs:351`, called from `src/cmd_agent.rs:7124`). One line for the local API, naming the addresses `api.json` lists (`api_health`, `src/mirror.rs:1132`). Then, for a wsp with no login, one line: `not logged in to a hub (standalone)`, and nothing else about hubs. For a spoke: the hub URL and user (from `hubs.json`); the certificate's expiry (read from a handshake with the URL's host, so a `tcp://` hub says it has none; a week or less is a warning); the last good sync and the last failure with its reason (`$WSP_STATE/hub/sync.json`, which the worker keeps); the journal's length; the row leases the claims here hold and the soonest expiry; the drive leases held. A lease within a day of its expiry whose seat is not alive is a warning, since nothing renews it. No host or address is a literal: everything is read from `api.json`, `hubs.json` and the worker's files.

## Later

Nothing here is built, and none of it is the design of record. It is what the first, retired design (one server for an org of groups; `git show fdade73:docs/multiplayer.md` has it whole, with its phases and breakdown) held that may come back on the hub, in the order it is likely to matter.

- **Groups and roles.** Groups, group leads and watchers above the hub's users and shared workspaces; today a workspace has members and the hub has admins.
- **Budgets and spend.** Spend charged to a shared workspace, with budgets that refuse a spawn past them; `spent.jsonl` would have to move into a store first.
- **Single sign-on.** A way to issue tokens from an identity provider; it would replace only how a token is issued.
- **Taking shared work back to private**, and a recorded take-over of another user's row.
- **A fence between users on a shared host.** A hub never hosts seats, so this matters only if two people share one machine.
- **Scale.** Several hub processes and a log per workspace, if one hub's single SQLite stops being enough.
