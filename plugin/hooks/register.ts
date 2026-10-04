// Task Journal mod (Claude Code 2.1.287+). A thin layer over the
// `task-journal` CLI: the Rust core stays the only writer of the journal.
//
// - keeps the session's active task in the system prompt, so a compaction
//   cannot lose it, and asks the compaction summary to keep its id;
// - stamps the session id on the journal's MCP calls, so each session has
//   its own active task;
// - shows the task in the status line and a toast on every entry;
// - reminds the agent to log only after N turns without an entry;
// - before a compaction, asks the model (from its own prompt cache) what
//   was not logged and records it as suggested events.
//
// It sets TJ_MOD_ACTIVE=1 for the hooks Claude Code starts, so the classic
// `task-journal ingest-hook` capture and `nudge` stand down instead of
// doing the same work twice.

import type { EngineInterface, Register } from 'claude-code'

import {
  compactInstruction,
  distillPrompt,
  journalTool,
  nudgeText,
  parseDistill,
  parseState,
  sectionText,
  statusText,
  transcriptExcerpt,
  WRITE_TOOLS,
  type JournalState,
  type TranscriptLine,
} from './journal'

const WORK_TOOLS = new Set(['Edit', 'Write', 'MultiEdit', 'NotebookEdit', 'Bash'])

// How much of the conversation the no-fork distill reads: the tail that
// fits a small model's prompt comfortably.
const EXCERPT_CHARS = 60_000

// What the mod knows about the session it runs in. A module variable: it
// lives as long as the process, which is as long as the session.
const mod = {
  isOff: false,
  hasWarned: false,
  state: null as JournalState | null,
  session: '',
  turnsSinceEntry: 0,
  workSinceEntry: 0,
}

async function cli($: EngineInterface, argv: string[]): Promise<string | null> {
  try {
    const run = await $.process.run(['task-journal', ...argv], {
      cwd: await $.session.root(),
      timeoutMs: 10_000,
    })

    return run.exitCode === 0 ? run.stdout : null
  } catch {
    return null
  }
}

// Reads the state of the session the engine names now. The id it was read
// for is kept even when the read fails, so a missing CLI costs one spawn
// per session, not one per request.
async function refresh($: EngineInterface): Promise<void> {
  const session = await $.session.id()
  const out = await cli($, ['state', '--session', session])
  const state = out === null ? null : parseState(out)

  mod.session = session
  mod.state = state

  if (state === null) {
    if (!mod.hasWarned) {
      mod.hasWarned = true
      $.ui.log(
        'task-journal: the task-journal CLI 0.30+ was not found on PATH, so the journal mod is off. Install it with `cargo install task-journal-cli task-journal-mcp --force`.',
      )
    }

    return
  }

  $.ui.status(statusText(state))
}

// The session id can change under the mod: a /clear starts a new one with
// no session.start, and a resumed session may only take its id after
// session.start ran. Re-read whenever the id moved.
async function current($: EngineInterface): Promise<JournalState | null> {
  if (mod.session !== (await $.session.id())) await refresh($)

  return mod.state
}

// Asks what the conversation decided that the journal lacks. A fork reuses
// the session's own cached transcript; a process that has sent nothing yet
// (a session resumed straight into /compact) has nothing to fork, and then
// the messages being compacted go to a small model instead.
async function distill($: EngineInterface, state: JournalState, messages: readonly TranscriptLine[]): Promise<void> {
  const task = state.active
  if (task === null) return

  const ask = distillPrompt(task)
  let reply = await $.model.fork({ prompt: ask })
  if (!reply.isAnswered && reply.reason === 'nothing-to-fork') {
    const conversation = transcriptExcerpt(messages, EXCERPT_CHARS)
    reply = await $.model.complete({
      model: 'haiku',
      prompt: `<conversation>\n${conversation}\n</conversation>\n\n${ask}`,
      maxTokens: 1024,
      timeoutMs: 60_000,
    })
  }
  if (!reply.isAnswered) return

  const session = await $.session.id()
  for (const event of parseDistill(reply.text, task.recent)) {
    await cli($, [
      'event', task.task_id,
      '--type', event.type,
      '--text', event.text,
      '--suggested',
      '--session', session,
      '--origin', 'mod-distill',
    ])
  }

  await refresh($)
}

export const register: Register = (on, options) => {
  const nudgeAfter = Number(options.nudge_after_turns ?? 6)
  const distillOnCompact = options.distill_on_compact !== false

  on('session.start', async ($, e, next) => {
    if ((await $.env.get('TJ_IN_CLASSIFIER')) !== undefined) {
      mod.isOff = true

      return next(e)
    }

    await refresh($)
    if (mod.state !== null) await $.env.set('TJ_MOD_ACTIVE', '1')

    return next(e)
  })

  on('session.end', async ($, e, next) => {
    mod.state = null
    mod.session = ''
    mod.turnsSinceEntry = 0
    mod.workSinceEntry = 0

    return next(e)
  })

  on('prompt.compose', async ($, e, next) => {
    const composed = await next(e)
    if (mod.isOff) return composed

    const state = await current($)
    if (state === null) return composed

    return {
      sections: [...composed.sections, { id: 'task-journal:active', text: sectionText(state), scope: 'session' }],
    }
  })

  on('prompt.submit', async ($, e, next) => {
    // Only the person's own prompts count as turns: not a background task's
    // notification, a peer's message or a plugin's prompt.
    const isPersons = e.origin === undefined || e.origin.kind === 'composer' || e.origin.kind === 'bridge'
    if (mod.isOff || !isPersons) return next(e)

    const state = await current($)
    if (state === null) return next(e)

    mod.turnsSinceEntry += 1
    const isDue =
      nudgeAfter > 0 && mod.turnsSinceEntry > nudgeAfter && (state.active !== null || mod.workSinceEntry > 0)
    if (!isDue) return next(e)

    mod.turnsSinceEntry = 0

    return next({ ...e, context: [...(e.context ?? []), nudgeText(state, nudgeAfter)] })
  })

  on('tool.call', async ($, e, next) => {
    const name = journalTool(String(e.tool))
    if (mod.isOff || name === undefined) {
      if (WORK_TOOLS.has(String(e.tool))) mod.workSinceEntry += 1

      return next(e)
    }

    // An MCP tool's arguments ride on `e` itself, untyped.
    const args = e as unknown as Record<string, unknown>
    const session = await $.session.id()
    const call = WRITE_TOOLS.has(name) && args.session_id === undefined ? { ...e, session_id: session } : e
    const ran = await next(call)
    if (ran.deny !== undefined || ran.isError === true || !WRITE_TOOLS.has(name)) return ran

    mod.turnsSinceEntry = 0
    mod.workSinceEntry = 0
    await refresh($)
    if (name === 'event_add' && typeof args.event_type === 'string') $.ui.toast(`📓 ${args.event_type} recorded`)

    return ran
  })

  on('session.compact', async ($, e, next) => {
    if (mod.isOff || e.agentId !== undefined) return next(e)

    const state = await current($)
    if (state === null || state.active === null) return next(e)

    if (distillOnCompact) {
      try {
        await distill($, state, e.messages)
      } catch {
        // The compaction matters more than the catch-up: go on without it.
      }
    }

    const keep = compactInstruction(state.active)

    return next({ ...e, instructions: e.instructions ? `${e.instructions}\n\n${keep}` : keep })
  })
}
