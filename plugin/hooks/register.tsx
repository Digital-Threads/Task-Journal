// Task Journal mod (Claude Code 2.1.287+). A thin layer over the
// `task-journal` CLI: the Rust core stays the only writer of the journal.
//
// - keeps the session's active task in the system prompt, so a compaction
//   cannot lose it, and asks the compaction summary to keep its id;
// - stamps the session id on the journal's MCP calls, so each session has
//   its own active task;
// - shows the task in a band above the prompt and a toast on every entry;
// - reminds the agent to log only after N turns without an entry;
// - brings up a project-chronicle gap that opened during the session (the
//   session start already showed the one it began with);
// - before a compaction, asks the model (from its own prompt cache) what
//   was not logged and records it as suggested events.
//
// It sets TJ_MOD_ACTIVE=1 for the hooks Claude Code starts, so the classic
// hooks skip what the mod does here (the reminder, per-message
// classification, the transcript catch-ups) and keep the rest.

import type { EngineInterface, Register } from 'claude-code'

import {
  chronicleKeys,
  chronicleNudge,
  compactInstruction,
  distillPrompt,
  journalTool,
  nudgeText,
  parseDistill,
  parseState,
  REFRESH_TOOLS,
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

// The longest a compaction waits for the catch-up before going on without it.
const DISTILL_TIMEOUT_MS = 60_000

// After a failed read (not a missing or old CLI), wait this long before
// trying again, so a broken journal costs one spawn a minute, not one a request.
const RETRY_AFTER_MS = 60_000

// What the mod knows about the session it runs in. A module variable: it
// lives as long as the process, which is as long as the session.
const mod = {
  isOff: false,
  hasWarned: false,
  // The directory the session started in. The journal's MCP server keeps
  // the directory it was started in too, so both resolve the same project
  // even after /cd or a worktree move.
  cwd: '',
  state: null as JournalState | null,
  session: '',
  retryAt: 0,
  // The task the system prompt names. It changes on task_create, on closing
  // it, or with the session — never because the agent logged to another
  // task — so the prompt (and the cache behind it) stays put.
  pinned: null as string | null,
  turnsSinceEntry: 0,
  workSinceEntry: 0,
  // Chronicle gaps this session has been told about. Seeded at its start
  // with every gap the SessionStart hook showed, so none is repeated.
  chronicleSeen: new Set<string>(),
}

type CliResult =
  | { kind: 'ok'; stdout: string }
  | { kind: 'missing' }
  | { kind: 'old' }
  | { kind: 'failed'; detail: string }

async function cli($: EngineInterface, argv: string[]): Promise<CliResult> {
  let run
  try {
    run = await $.process.run(['task-journal', ...argv], {
      cwd: mod.cwd || (await $.session.cwd()),
      timeoutMs: 30_000,
    })
  } catch (err) {
    const detail = String(err)

    return /ENOENT|not found|No such file/i.test(detail) ? { kind: 'missing' } : { kind: 'failed', detail }
  }

  if (run.exitCode === 0) return { kind: 'ok', stdout: run.stdout }
  if (/unrecognized subcommand|unexpected argument/i.test(run.stderr)) return { kind: 'old' }

  return { kind: 'failed', detail: run.stderr.trim().split('\n')[0] ?? `exit ${run.exitCode}` }
}

function warnOnce($: EngineInterface, result: CliResult): void {
  if (mod.hasWarned || result.kind === 'ok') return
  mod.hasWarned = true

  const text =
    result.kind === 'missing'
      ? 'task-journal: the `task-journal` CLI is not on PATH, so the journal mod is off. Install it with `cargo install task-journal-cli task-journal-mcp`.'
      : result.kind === 'old'
        ? 'task-journal: the installed `task-journal` CLI is older than this plugin (0.30+ needed), so the journal mod is off. Update it with `cargo install task-journal-cli task-journal-mcp --force`.'
        : `task-journal: reading the journal failed (${result.detail}); the mod will retry in a minute.`
  $.ui.log(text)
}

// Reads the state of the session the engine names now. A missing or old CLI
// is final for the session; any other failure is retried after a pause.
async function refresh($: EngineInterface): Promise<void> {
  const session = await $.session.id()
  const prefer = mod.session === session && mod.pinned !== null ? ['--prefer', mod.pinned] : []
  const result = await cli($, ['state', '--session', session, ...prefer])
  const state = result.kind === 'ok' ? parseState(result.stdout) : null

  if (state === null) {
    warnOnce($, result)
    mod.state = null
    $.ui.invalidate('ui.render')
    if (result.kind === 'failed') {
      mod.retryAt = (await $.clock.now()) + RETRY_AFTER_MS
    } else {
      mod.session = session
    }

    return
  }

  if (mod.session !== session) {
    mod.pinned = null
    mod.chronicleSeen = new Set(chronicleKeys(state))
  }
  mod.session = session
  mod.state = state
  mod.pinned = state.active?.task_id ?? null
  $.ui.invalidate('ui.render')
}

// The session id can change under the mod: a /clear starts a new one with
// no session.start, and a resumed session may only take its id after
// session.start ran. Re-read whenever the id moved, or a retry is due.
async function current($: EngineInterface): Promise<JournalState | null> {
  const session = await $.session.id()
  const isRetryDue = mod.state === null && mod.retryAt > 0 && (await $.clock.now()) >= mod.retryAt
  if (mod.session !== session || isRetryDue) {
    mod.retryAt = 0
    await refresh($)
  }

  return mod.state
}

// Resolves to undefined once `ms` passed. The wait costs the hook nothing
// while the raced call is in flight.
async function within<T>($: EngineInterface, ms: number, call: Promise<T>): Promise<T | undefined> {
  let timer: { cancel: () => void } | undefined
  const timeout = new Promise<undefined>(resolve => {
    timer = $.clock.after(ms, () => resolve(undefined))
  })

  try {
    return await Promise.race([call, timeout])
  } finally {
    timer?.cancel()
  }
}

// Asks what the conversation decided that the journal lacks. A fork reuses
// the session's own cached transcript; a process that has sent nothing yet
// (a session resumed straight into /compact) has nothing to fork, and then
// the messages being compacted go to a small model instead.
async function distill($: EngineInterface, state: JournalState, messages: readonly TranscriptLine[]): Promise<void> {
  const task = state.active
  if (task === null) return

  const ask = distillPrompt(task)
  let reply = await within($, DISTILL_TIMEOUT_MS, $.model.fork({ prompt: ask }))
  if (reply !== undefined && !reply.isAnswered && reply.reason === 'nothing-to-fork') {
    const conversation = transcriptExcerpt(messages, EXCERPT_CHARS)
    reply = await $.model.complete({
      model: 'haiku',
      prompt: `<conversation>\n${conversation}\n</conversation>\n\n${ask}`,
      maxTokens: 1024,
      timeoutMs: DISTILL_TIMEOUT_MS,
    })
  }
  if (reply === undefined || !reply.isAnswered) return

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

    mod.cwd = e.cwd
    await refresh($)
    if (mod.state !== null) await $.env.set('TJ_MOD_ACTIVE', '1')

    return next(e)
  })

  on('session.end', async ($, e, next) => {
    mod.state = null
    mod.session = ''
    mod.pinned = null
    mod.retryAt = 0
    mod.turnsSinceEntry = 0
    mod.workSinceEntry = 0
    mod.chronicleSeen = new Set()

    return next(e)
  })

  // A band above the prompt rather than `$.ui.status`: the engine draws that
  // line as one of its pinned warnings, yellow with a ⚠, which reads as
  // something being wrong. The band holds one tree for every plugin, so the
  // line goes on top of what the hooks beneath draw (another mod's band).
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const text = mod.isOff || mod.state === null ? null : statusText(mod.state)
    if (text === null || e.props.hasSurvey) return next(e)

    const { Box, Text } = $.ui.resolve(e)
    const below = await next(e)

    return (
      <Box flexDirection="column">
        <Text dimColor>{text}</Text>
        {below}
      </Box>
    )
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

    // Both reminders ride in the prompt's context, never in the system
    // prompt: a change there would re-send the conversation uncached.
    const added: string[] = []
    const chronicle = chronicleNudge(state, mod.chronicleSeen)
    if (chronicle !== null) {
      mod.chronicleSeen.add(chronicle.key)
      added.push(chronicle.text)
    }

    mod.turnsSinceEntry += 1
    const isDue =
      nudgeAfter > 0 && mod.turnsSinceEntry > nudgeAfter && (state.active !== null || mod.workSinceEntry > 0)
    if (isDue) {
      mod.turnsSinceEntry = 0
      added.push(nudgeText(state, nudgeAfter))
    }
    if (added.length === 0) return next(e)

    return next({ ...e, context: [...(e.context ?? []), ...added] })
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
    if (ran.deny !== undefined || ran.isError === true || !REFRESH_TOOLS.has(name)) return ran
    if (!WRITE_TOOLS.has(name)) {
      await refresh($)

      return ran
    }

    // A new task, or closing the named one, moves the pin; logging to
    // another task does not.
    if (name === 'task_create' || (name === 'task_close' && args.task_id === mod.pinned)) mod.pinned = null

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

    // A precompute builds a summary ahead of time that may never be used;
    // the catch-up waits for a compaction that happens.
    if (distillOnCompact && e.trigger !== 'precompute') {
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
