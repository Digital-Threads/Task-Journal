// Pure helpers of the Task Journal mod: everything here is plain data in,
// plain data out, so journal.test.ts covers it without an engine.

export type JournalEvent = { type: string; text: string }

export type ActiveTask = {
  task_id: string
  title: string
  goal: string | null
  counts: Record<string, number>
  recent: JournalEvent[]
}

export type JournalState = {
  active: ActiveTask | null
  open_tasks: number
}

export const STATE_SCHEMA = 'tj-state/1'

const DISTILL_TYPES = new Set(['decision', 'rejection', 'finding', 'evidence', 'constraint', 'hypothesis'])

const MAX_DISTILLED = 8

const MAX_EVENT_CHARS = 500

// `mcp__plugin_task-journal_task-journal__event_add` when the server comes
// with the plugin, `mcp__task-journal__event_add` when it was wired by hand.
const JOURNAL_TOOL = /^mcp__(?:plugin_task-journal_)?task-journal__([a-z_]+)$/

export const WRITE_TOOLS = new Set(['task_create', 'event_add', 'artifact_add', 'task_close'])

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
  const a = v.active as Record<string, unknown> | null | undefined
  if (a === null || a === undefined || typeof a.task_id !== 'string') {
    return { active: null, open_tasks: open }
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
    },
    open_tasks: open,
  }
}

export function statusText(state: JournalState): string {
  const a = state.active
  if (a === null) {
    return state.open_tasks > 0 ? `📓 no task in this session · ${state.open_tasks} open` : '📓 no task'
  }

  const n = (k: string) => a.counts[k] ?? 0

  return `📓 ${a.task_id} · ${n('decision')} decisions · ${n('rejection')} rejected · ${n('evidence')} evidence`
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
    `Each one sentence, specific (names, files, numbers). At most ${MAX_DISTILLED}. Nothing worth adding → [].`,
    'Reply with a JSON array only: [{"type": "decision", "text": "..."}]. Allowed types: decision, rejection, finding, evidence, constraint, hypothesis.',
  ].join('\n')
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
