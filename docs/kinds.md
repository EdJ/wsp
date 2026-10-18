# Agent kinds as files

wsp has behaviour for three agent runtimes written in Rust (`claude`, `opencode`,
`pi`; `agent_commands::KNOWN`). Any other runtime can be described by a file,
with no rebuild and no reinstall. This page is what such a file may say, what it
may not, and why.

This is the *registry* half of wsp-113 ("reading 1"). A kind declares facts. It
does not yet declare a way of being reached — see [What is not built](#what-is-not-built).

## Where

```
<store>/kinds/<name>.json        # <store> is $WSP_HOME, or ~/wsp
```

One file per kind; the file name without `.json` must equal the `name` inside.
The directory is read once per process, files in name order. A missing
directory is an empty registry. `wsp agent kinds` lists built-in kinds, then
data kinds, then any file that was refused with the reason (`"refused"` in
`--json`).

A name resolves in this order: the built-ins, then the registry, then `Plain`
(unknown to wsp, passed through to herdr, which owns the catalogue). A built-in
always wins; a file cannot shadow one.

## The shape

```json
{
  "kind_version": 1,
  "name": "mytool",
  "args": ["--yes", "--no-update-check"],
  "resume_flag": "--continue",
  "clear": { "cmd": "/new", "replaces_session": false },
  "briefed": true,
  "order_in_args": false,
  "brief_file": false,
  "queue_is_the_agents": true
}
```

Only `kind_version` and `name` are required.

| Key | Meaning | Default |
|---|---|---|
| `kind_version` | must be the number `1` | required |
| `name` | 1–32 of `a-z`, `0-9`, `-`; equals the file stem; not a built-in | required |
| `args` | argv words after the binary, strings only | `[]` |
| `resume_flag` | the flag that takes a session id, e.g. `--continue`; wsp appends flag then id | none: `wsp resume` refuses rather than starting a fresh session |
| `clear` | `{cmd, replaces_session}`: what typed into the pane empties its context, and whether a new session id follows (wsp waits for it) | none: no clear is guessed |
| `briefed` | the agent arrives knowing its task (a hook or the like runs `wsp brief`) | `false` |
| `order_in_args` | the work order is the last argv word, so wsp does not also type it | `false` |
| `brief_file` | the composed brief is handed over as a file, not in argv (herdr refuses control characters in argv) | `false` |
| `queue_is_the_agents` | a sentence sent mid-turn is durably queued by the agent and answered at the turn boundary, so wsp need not hold it | `true` |

No templating: nothing declared is interpolated into anything.

## Refusals

The file is a compatibility surface. A file that is wrong is **refused with its
reason, never ignored and never half-read**: not valid JSON; not an object;
`kind_version` missing or not `1`; an unknown key (also inside `clear`); a bad
`name`; a built-in's name; a `name` that does not match the file name; a field
of the wrong type. A refused kind is not registered. `wsp spawn --kind NAME`
for a name whose file was refused errors with the reason, rather than quietly
passing `NAME` to herdr as if the file did not exist. A name with no file at all
is still passed through.

Unknown keys are refused so that a file written for a later `kind_version`
cannot be half-honoured by an earlier wsp.

## What a data kind cannot declare

These are absent on purpose. Each is a fact wsp would have to *believe* rather
than read, and a wrong belief here is delivered to a live agent.

- **A model or effort vocabulary.** wsp's tier names are measured per kind. A
  data kind refuses `--model` and `--effort` as `Plain` does, rather than pass
  an alias it cannot check.
- **An address.** wsp never guesses a handle to reach an agent by. A data kind
  has none, so it is never told by anything but its pane.
- **Delivery.** `Delivery` stays honest: a data kind delivers by typing at the
  pane, and typing at a terminal is only the fallback, so a delivery can remain
  `Unconfirmed` and is reported so. A file cannot claim a sentence was received.
- **Liveness, a transcript, or spend.** There is no `running` and no `ran`, so
  wsp reports these as unknown, not as zero. (A `$0` that is really "unmeasured"
  is the failure this avoids.)
- **Environment.** Nothing is set in the agent's environment.
- **A hook or program wsp would run.** The file is data. It names words for
  argv and for the pane; it executes nothing.

And one rule that holds for every kind, declared or built-in: nothing types where
a person is. A pane someone is using is not typed into on a file's say-so.

## What is not built

Reading 2 of wsp-113 — a kind, or a running agent, declaring *its own way of
being reached* (an MCP server, a socket, a queue) so that wsp asks rather than
types — is not built. Neither is reading 3, a transport per seat, which was
argued against: it makes "who is in this seat" a question with two answers. A
data kind today adds a runtime to what wsp can start, resume and clear. It adds
no transport.

The external handbook needs a matching line pointing here; it lives outside this
repository.
