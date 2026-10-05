// Pure helpers of the Task Journal mod: everything here is plain data in,
// plain data out, so journal.test.ts covers it without an engine.

export type JournalEvent = { type: string; text: string }

export type ActiveTask = {
  task_id: string
  title: string
  goal: string | null
  counts: Record<string, number>
  recent: JournalEvent[]
  /** Names of the modules the task belongs to. */
  modules: string[]
}

/** What the project chronicle is missing: `kind` plus its own fields. */
export type ArchiveGap = { kind: string; [field: string]: unknown }

export type JournalState = {
  active: ActiveTask | null
  open_tasks: number
  /** Most important first. Empty from a CLI older than 0.31. */
  archive: ArchiveGap[]
}

export const STATE_SCHEMA = 'tj-state/1'

const DISTILL_TYPES = new Set(['decision', 'rejection', 'finding', 'evidence', 'constraint', 'hypothesis'])

const MAX_DISTILLED = 8

const MAX_EVENT_CHARS = 500

// `mcp__plugin_task-journal_task-journal__event_add` when the server comes
// with the plugin, `mcp__task-journal__event_add` when it was wired by hand.
const JOURNAL_TOOL = /^mcp__(?:plugin_task-journal_)?task-journal__([a-z_]+)$/

export const WRITE_TOOLS = new Set(['task_create', 'event_add', 'artifact_add', 'task_close'])

// After these the status line is re-read: the journal writes, and the module
// tools that change which modules a task shows.
export const REFRESH_TOOLS = new Set([...WRITE_TOOLS, 'module_link', 'module_save'])

export function journalTool(tool: string): string | undefined {
  return JOURNAL_TOOL.exec(tool)?.[1]
}

/** The `task-journal state --json` output, or null when it is not one. */
export function parseState(stdout: string): JournalState | null {
  let raw: unknown
  try {
    raw = JSON.parse(stdout)
  } catch {
    return null
  }

  if (typeof raw !== 'object' || raw === null) return null
  const v = raw as Record<string, unknown>
  if (v.schema !== STATE_SCHEMA) return null

  const open = typeof v.open_tasks === 'number' ? v.open_tasks : 0
  const archive = Array.isArray(v.archive)
    ? v.archive.filter(
        (g): g is ArchiveGap => typeof g === 'object' && g !== null && typeof (g as ArchiveGap).kind === 'string',
      )
    : []
  const a = v.active as Record<string, unknown> | null | undefined
  if (a === null || a === undefined || typeof a.task_id !== 'string') {
    return { active: null, open_tasks: open, archive }
  }

  const recent = Array.isArray(a.recent)
    ? a.recent.filter(
        (r): r is JournalEvent =>
          typeof r === 'object' && r !== null && typeof r.type === 'string' && typeof r.text === 'string',
      )
    : []

  return {
    active: {
      task_id: a.task_id,
      title: typeof a.title === 'string' ? a.title : a.task_id,
      goal: typeof a.goal === 'string' && a.goal.trim() !== '' ? a.goal : null,
      counts: (a.counts as Record<string, number>) ?? {},
      recent,
      modules: Array.isArray(a.modules) ? a.modules.filter((m): m is string => typeof m === 'string') : [],
    },
    open_tasks: open,
    archive,
  }
}

// The band above the prompt: the session's task, or nothing while it has none.
export function statusText(state: JournalState): string | null {
  const a = state.active
  if (a === null) return null

  const n = (k: string) => a.counts[k] ?? 0
  const where = a.modules.length > 0 ? ` [${a.modules.join(', ')}]` : ''

  return `📓 ${a.task_id}${where} · ${n('decision')} decisions · ${n('rejection')} rejected · ${n('evidence')} evidence`
}

const GAP_TEXT: Record<string, (g: ArchiveGap) => string> = {
  no_map: g =>
    `no module map yet (${g.tasks} tasks) — map the project with /task-journal:map and confirm the modules with the user`,
  unlinked_tasks: g =>
    `${g.count} task(s) belong to no module — sort them with module_backfill_candidates, confirm with the user, then module_link (leftovers go to a catch-all module)`,
  stale_module: g =>
    `module ${g.module_id}: ${g.closed_since} closed task(s) not reflected in its state — read module_page and rewrite it with module_save(state=...)`,
  task_without_module: g => `the active task ${g.task_id} belongs to no module — link it with module_link (module_list shows the map)`,
}

// The field that tells one gap of a kind from another; counts are left out,
// so a gap whose count moved is not new.
const GAP_SUBJECT: Record<string, string> = { stale_module: 'module_id', task_without_module: 'task_id' }

function gapKey(gap: ArchiveGap): string {
  const subject = GAP_SUBJECT[gap.kind]

  return subject === undefined ? gap.kind : `${gap.kind}:${String(gap[subject])}`
}

/** Keys of the gaps a state names: what the session start already showed. */
export function chronicleKeys(state: JournalState): string[] {
  return state.archive.filter(g => GAP_TEXT[g.kind] !== undefined).map(gapKey)
}

/**
 * The chronicle gap to bring up, if one is new to this session (`seen`
 * holds the keys already shown). The session's own task without a module
 * comes first: it is the one gap the agent can close right now.
 */
export function chronicleNudge(
  state: JournalState,
  seen: ReadonlySet<string> = new Set(),
): { key: string; text: string } | null {
  const fresh = state.archive.filter(g => GAP_TEXT[g.kind] !== undefined && !seen.has(gapKey(g)))
  const gap = fresh.find(g => g.kind === 'task_without_module') ?? fresh[0]
  const describe = gap === undefined ? undefined : GAP_TEXT[gap.kind]
  if (gap === undefined || describe === undefined) return null

  return { key: gapKey(gap), text: `📚 Chronicle: ${describe(gap)}. Close this gap once the current work is done.` }
}

/**
 * The system-prompt section. It sits before the conversation, so every
 * change re-sends the whole conversation uncached: it names only what
 * changes rarely (the task, its title and goal), never counts or times.
 */
export function sectionText(state: JournalState): string {
  const a = state.active
  if (a === null) {
    return [
      'Task Journal: no journal task is open in this session.',
      'When real work starts, resume one with task_search(status="open") or open one with task_create(goal=...).',
    ].join('\n')
  }

  return [
    `Task Journal: the active task in this session is ${a.task_id} — "${a.title}".`,
    `Goal: ${a.goal ?? 'not set yet — pass it when you next call task_create, or ask the user.'}`,
    `Record each decision, rejection, finding and test result with event_add(task_id="${a.task_id}") the moment it happens; close it with task_close when the work is done.`,
  ].join('\n')
}

export function nudgeText(state: JournalState, turns: number): string {
  const a = state.active
  if (a === null) {
    return 'Task Journal: work is happening but no journal task is open in this session. Resume one with task_search(status="open") or open one with task_create(goal=...).'
  }

  return `Task Journal: ${turns} turns since the last journal entry on ${a.task_id}. If a decision, rejection, finding or test result happened since, record it now with event_add.`
}

export function compactInstruction(task: ActiveTask): string {
  return `Keep the Task Journal task id ${task.task_id} ("${task.title}") and its goal in the summary, so the work can go on logging to it.`
}

export function distillPrompt(task: ActiveTask): string {
  const known = task.recent.map(r => `- [${r.type}] ${r.text}`).join('\n') || '- (nothing yet)'

  return [
    `You are auditing the Task Journal of task ${task.task_id} ("${task.title}").`,
    `Goal: ${task.goal ?? 'not set'}.`,
    'Already recorded:',
    known,
    '',
    'From the conversation above, list the decisions, rejections, findings, evidence, constraints and hypotheses that matter for this task and are NOT already recorded.',
    'Do not restate an entry already recorded in other words, and do not turn a recorded decision into a hypothesis or finding.',
    `Each one sentence, specific (names, files, numbers). At most ${MAX_DISTILLED}. Nothing worth adding → [].`,
    'Reply with a JSON array only: [{"type": "decision", "text": "..."}]. Allowed types: decision, rejection, finding, evidence, constraint, hypothesis.',
  ].join('\n')
}

export type TranscriptLine = { role: string; text: string }

/**
 * The newest turns of a conversation as `role: text` lines, cut to
 * `maxChars` from the end: what a model reads when it cannot fork the
 * session's own cached transcript.
 */
export function transcriptExcerpt(messages: readonly TranscriptLine[], maxChars: number): string {
  const lines: string[] = []
  let size = 0

  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]
    const text = m?.text.trim() ?? ''
    if (m === undefined || text === '') continue

    const line = `${m.role}: ${text}`
    if (size + line.length > maxChars) {
      if (lines.length === 0) lines.push(line.slice(line.length - maxChars))
      break
    }

    lines.push(line)
    size += line.length + 1
  }

  return lines.reverse().join('\n')
}

function normalized(text: string): string {
  return text.toLowerCase().replace(/\s+/g, ' ').trim()
}

/** The model's distill reply → the events worth recording, already-known ones dropped. */
export function parseDistill(reply: string, known: JournalEvent[]): JournalEvent[] {
  const start = reply.indexOf('[')
  const end = reply.lastIndexOf(']')
  if (start === -1 || end <= start) return []

  let raw: unknown
  try {
    raw = JSON.parse(reply.slice(start, end + 1))
  } catch {
    return []
  }
  if (!Array.isArray(raw)) return []

  const seen = new Set(known.map(k => normalized(k.text)))
  const out: JournalEvent[] = []

  for (const item of raw) {
    if (typeof item !== 'object' || item === null) continue
    const { type, text } = item as Record<string, unknown>
    if (typeof type !== 'string' || typeof text !== 'string') continue
    if (!DISTILL_TYPES.has(type)) continue

    const clean = text.trim().slice(0, MAX_EVENT_CHARS)
    if (clean === '' || seen.has(normalized(clean))) continue

    seen.add(normalized(clean))
    out.push({ type, text: clean })
    if (out.length === MAX_DISTILLED) break
  }

  return out
}
