// installed by wsp
// managed by wsp: `wsp install` overwrites this file with the copy the
// installed binary carries (`wsp agent pi-extension` prints it), and
// `wsp doctor` says when the two differ.
// @ts-nocheck
//
// wsp ↔ pi: let a pi that wsp is hosting say what it is doing.
//
// pi has no hooks; this is claude-code/wsp-session.sh for an agent whose only
// way in is an extension. It turns pi's own lifecycle into the hook names
// `wsp report` already reads (src/place_super.rs `said_by`), so nothing on the
// wsp side has to know which agent is talking:
//
// | pi event                     | wsp report       | read as |
// |------------------------------|------------------|---------|
// | session_start                | SessionStart     | idle    |
// | agent_start                  | UserPromptSubmit | working |
// | before_agent_start (user)    | compound-sup submitted: the delivery's ack |
// | tool_execution_start / _end  | PreToolUse / PostToolUse | still working (a sign of life) |
// | agent_settled, and idle      | Stop             | idle    |
// | ui_prompt_start              | PermissionRequest| working |
// | ui_prompt_end                | what it was before the prompt |
// | session_shutdown, on quit    | SessionEnd       | gone    |
//
// Installed *beside* herdr's herdr-agent-state.ts rather than into it — that
// file says in its header that herdr overwrites it — and independent of it:
// the two read the same events, neither listens for the other, and a pane
// can have both, either or neither.
//
// The brief is not here. Claude's SessionStart hook prints it into context;
// a pi is handed its brief on the command line by whatever launches it, which
// is wsp's spawn and not this file.
//
// Everything here is best-effort: an extension that fails a session, or
// delays one, is an extension that gets deleted within a week. Nothing throws,
// nothing prints, and no handler waits on wsp except the last one.

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const seat = process.env.WSP_SEAT_ID;

// pi hands every command its bash tool runs the session it is running in, as
// PI_SESSION_ID. Finding it here means this pi was started by another pi's
// shell — inside the seat, and inheriting WSP_SEAT_ID with it — and is not the
// agent the seat is for. Its turns ending must not tell wsp the seat is idle
// while the pi above it is mid-turn: Claude's hook drops a subagent for the
// same reason. wsp sheds PI_SESSION_ID from a seat it starts (`place::shed`),
// so a pi governor spawning a pi member does not trip this.
const nested = !!process.env.PI_SESSION_ID;

function enabled() {
  return !!seat && !nested;
}

// One `wsp report` at a time, in the order pi said them. Each is its own
// process, and two racing would let a turn's Stop land before its
// UserPromptSubmit and leave the seat reading working all night.
let queue: Promise<void> = Promise.resolve();

function run(hook: string, payload: Record<string, unknown>): Promise<void> {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn("wsp", ["report", hook], { stdio: ["pipe", "ignore", "ignore"] });
    } catch {
      resolve();
      return;
    }
    // A wsp that hangs must not hold every later report behind it.
    const timer = setTimeout(() => {
      try {
        child.kill();
      } catch {}
      resolve();
    }, 5000);
    timer.unref?.();
    const done = () => {
      clearTimeout(timer);
      resolve();
    };
    // No wsp on PATH is ENOENT here, and the same silence as the hook's
    // `command -v wsp || exit 0`.
    child.on("error", done);
    child.on("close", done);
    child.stdin?.on("error", () => {});
    child.stdin?.end(JSON.stringify(payload));
  });
}

// What claude-code/wsp-session.sh's `submitted` does for a Claude seat: tell the
// seat's supervisor the prompt was submitted, which acknowledges a
// `compound-sup deliver` exactly once. The seat's session socket is in its
// record, not its environment. Best-effort and silent like everything here: no
// sup, no record, no socket, a seat on another machine — nothing to say.
function submitted(prompt: string) {
  try {
    const state = process.env.WSP_STATE || join(homedir(), ".local", "state", "wsp");
    const rec = JSON.parse(readFileSync(join(state, "compound-seats", seat as string, "seat.json"), "utf8"));
    if (rec?.machine || typeof rec?.socket !== "string" || rec.socket.length === 0) {
      return;
    }
    const child = spawn("compound-sup", ["submitted", rec.socket], { stdio: ["pipe", "ignore", "ignore"] });
    const timer = setTimeout(() => {
      try {
        child.kill();
      } catch {}
    }, 5000);
    timer.unref?.();
    child.on("error", () => clearTimeout(timer));
    child.on("close", () => clearTimeout(timer));
    child.stdin?.on("error", () => {});
    child.stdin?.end(JSON.stringify({ prompt }));
  } catch {}
}

function report(hook: string, ctx: any): Promise<void> {
  // The session id is the one payload key wsp keeps from pi. Not
  // `transcript_path`: wsp reads that file as Claude Code's transcript to tally
  // a seat's burn, and pi's session file names its usage differently, so it
  // would count every turn at nothing.
  let session_id: string | undefined;
  try {
    const id = ctx?.sessionManager?.getSessionId?.();
    session_id = typeof id === "string" && id.length > 0 ? id : undefined;
  } catch {}
  const payload = { hook_event_name: hook, session_id };
  queue = queue.then(() => run(hook, payload));
  return queue;
}

export default function (pi) {
  if (!enabled()) {
    return;
  }

  let prompts = 0;

  // What the seat is doing when no prompt is up: a turn running, or not.
  function underneath(ctx: any) {
    return ctx?.isIdle?.() === false ? "UserPromptSubmit" : "Stop";
  }

  pi.on("session_start", (_event, ctx) => {
    // The launch window closes here. A reload replaces this extension mid-run
    // without another agent_start, so a turn already underway is said too.
    void report("SessionStart", ctx);
    if (ctx?.isIdle?.() === false) {
      void report("UserPromptSubmit", ctx);
    }
  });

  // The prompt is only on this event; the ack goes before the turn is said to
  // have begun, as Claude's hook orders them.
  pi.on("before_agent_start", (event) => {
    if (event?.messageType === "user" && typeof event?.prompt === "string") {
      submitted(event.prompt);
    }
  });

  // pi runs tools for minutes inside one turn and says nothing else while it
  // does. A record that is not refreshed ages out after two minutes and the
  // seat reads unknown, and `wsp tell` refuses it. A tool starting or ending is
  // a sign of life; `wsp report` re-dates a working record and changes nothing
  // else. Throttled: a burst of tool calls is one report, not a hundred
  // processes.
  let lastTool = 0;
  function tool(hook: string, ctx: any) {
    const now = Date.now();
    if (now - lastTool < 5000) {
      return;
    }
    lastTool = now;
    void report(hook, ctx);
  }
  pi.on("tool_execution_start", (_event, ctx) => {
    tool("PreToolUse", ctx);
  });
  pi.on("tool_execution_end", (_event, ctx) => {
    tool("PostToolUse", ctx);
  });

  pi.on("agent_start", (_event, ctx) => {
    void report("UserPromptSubmit", ctx);
  });

  // agent_settled, not agent_end: pi may still retry, compact or run queued
  // work after agent_end, and only settled means it will not carry on alone.
  pi.on("agent_settled", (_event, ctx) => {
    if (ctx?.isIdle?.() !== true) {
      return;
    }
    void report("Stop", ctx);
  });

  // pi asks nothing of its own — no permission prompt, measured for
  // pi-support-002 — so the one way a person becomes needed is an extension
  // putting a dialog up. That is what PermissionRequest means to wsp.
  pi.on("ui_prompt_start", (_event, ctx) => {
    prompts += 1;
    void report("PermissionRequest", ctx);
  });

  // Claude Code has no "the question was answered"; its next hook says it.
  // pi says it here, so the raised hand is put down at once rather than at the
  // end of the turn.
  pi.on("ui_prompt_end", (_event, ctx) => {
    prompts = Math.max(0, prompts - 1);
    if (prompts === 0) {
      void report(underneath(ctx), ctx);
    }
  });

  // Only on quit. reload, new, resume and fork tear this runtime down and
  // start another in the same process, whose session_start follows at once;
  // the agent has not gone. This is the one handler pi waits on, so that the
  // last word reaches wsp before the process exits — bounded, because a quit
  // that hangs on wsp is worse than a seat that reads gone a tick late.
  pi.on("session_shutdown", async (event, ctx) => {
    if (event?.reason !== "quit") {
      return;
    }
    await Promise.race([report("SessionEnd", ctx), new Promise((r) => setTimeout(r, 1500).unref?.())]);
  });
}
