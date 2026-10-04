import { describe, expect, test } from 'claude-code/testing'

import {
  chronicleNudge,
  journalTool,
  nudgeText,
  parseDistill,
  parseState,
  sectionText,
  statusText,
  transcriptExcerpt,
  type JournalState,
} from './journal'

const ACTIVE = JSON.stringify({
  schema: 'tj-state/1',
  open_tasks: 2,
  active: {
    task_id: 'tj-abc',
    title: 'Fix refresh',
    goal: 'Stop dropping the token',
    counts: { decision: 3, rejection: 1, evidence: 0 },
    recent: [{ type: 'decision', text: 'Use <= on expiry' }],
  },
})

describe('parseState', () => {
  test('reads the active task', () => {
    const s = parseState(ACTIVE)
    expect(s?.active?.task_id).toBe('tj-abc')
    expect(s?.active?.recent.length).toBe(1)
    expect(s?.open_tasks).toBe(2)
  })

  test('no active task is a state, not a failure', () => {
    const s = parseState(JSON.stringify({ schema: 'tj-state/1', open_tasks: 0, active: null }))
    expect(s).toEqual({ active: null, open_tasks: 0, archive: [] })
  })

  test('rejects garbage and other schemas', () => {
    expect(parseState('error: unrecognized subcommand')).toBe(null)
    expect(parseState(JSON.stringify({ schema: 'tj-state/9', active: null }))).toBe(null)
  })

  test('an empty goal reads as not set', () => {
    const s = parseState(JSON.stringify({ schema: 'tj-state/1', open_tasks: 1, active: { task_id: 'tj-x', goal: ' ' } }))
    expect(s?.active?.goal).toBe(null)
    expect(s?.active?.title).toBe('tj-x')
  })
})

describe('texts', () => {
  const s = parseState(ACTIVE) as JournalState
  const none: JournalState = { active: null, open_tasks: 3, archive: [] }

  test('status line names the task and its counts', () => {
    expect(statusText(s)).toBe('📓 tj-abc · 3 decisions · 1 rejected · 0 evidence')
    expect(statusText(none)).toBe('📓 no task in this session · 3 open')
    expect(statusText({ active: null, open_tasks: 0, archive: [] })).toBe('📓 no task')
  })

  test('the prompt section holds nothing that changes per entry', () => {
    const text = sectionText(s)
    expect(text).toContain('tj-abc')
    expect(text).toContain('Stop dropping the token')
    expect(text).not.toContain('3 decisions')
    expect(text).not.toContain('Use <= on expiry')
  })

  test('nudge names the task, or asks to open one', () => {
    expect(nudgeText(s, 6)).toContain('6 turns since the last journal entry on tj-abc')
    expect(nudgeText(none, 6)).toContain('task_create')
  })
})

describe('chronicle', () => {
  const withGaps = (archive: unknown[], modules: string[] = []) =>
    parseState(
      JSON.stringify({
        schema: 'tj-state/1',
        open_tasks: 1,
        archive,
        active: { task_id: 'tj-a', title: 'T', goal: null, counts: {}, recent: [], modules },
      }),
    ) as JournalState

  test('status line names the task modules', () => {
    expect(statusText(withGaps([], ['Stars', 'Auth']))).toBe(
      '📓 tj-a [Stars, Auth] · 0 decisions · 0 rejected · 0 evidence',
    )
  })

  test('an older CLI without archive or modules parses as none', () => {
    const s = parseState(ACTIVE) as JournalState
    expect(s.archive).toEqual([])
    expect(s.active?.modules).toEqual([])
  })

  test('the active task without a module comes first', () => {
    const s = withGaps([
      { kind: 'unlinked_tasks', count: 4 },
      { kind: 'task_without_module', task_id: 'tj-a' },
    ])
    const n = chronicleNudge(s)
    expect(n?.key).toBe('task_without_module:tj-a')
    expect(n?.text).toContain('📚 Chronicle: the active task tj-a belongs to no module')
  })

  test('otherwise the first gap, keyed so a changed count is not new', () => {
    const a = chronicleNudge(withGaps([{ kind: 'unlinked_tasks', count: 4 }]))
    const b = chronicleNudge(withGaps([{ kind: 'unlinked_tasks', count: 5 }]))
    expect(a?.key).toBe(b?.key)
    expect(a?.text).toContain('module_backfill_candidates')
    expect(chronicleNudge(withGaps([{ kind: 'no_map', tasks: 3 }]))?.text).toContain('/task-journal:map')
    expect(chronicleNudge(withGaps([{ kind: 'stale_module', module_id: 'stars', closed_since: 2 }]))?.key).toBe(
      'stale_module:stars',
    )
  })

  test('no gaps, or only unknown ones, is no nudge', () => {
    expect(chronicleNudge(withGaps([]))).toBe(null)
    expect(chronicleNudge(withGaps([{ kind: 'from_the_future' }]))).toBe(null)
  })
})

describe('journalTool', () => {
  test('matches the plugin server and a hand-wired one', () => {
    expect(journalTool('mcp__plugin_task-journal_task-journal__event_add')).toBe('event_add')
    expect(journalTool('mcp__task-journal__task_close')).toBe('task_close')
    expect(journalTool('mcp__other__event_add')).toBe(undefined)
    expect(journalTool('Bash')).toBe(undefined)
  })
})

describe('parseDistill', () => {
  const known = [{ type: 'decision', text: 'Use <= on expiry' }]

  test('keeps typed, new events from a fenced reply', () => {
    const reply = 'Here:\n```json\n[{"type":"rejection","text":"Ruled out a cache: stale tokens"},{"type":"finding","text":"refresh.rs:42 uses <"}]\n```'
    expect(parseDistill(reply, known)).toEqual([
      { type: 'rejection', text: 'Ruled out a cache: stale tokens' },
      { type: 'finding', text: 'refresh.rs:42 uses <' },
    ])
  })

  test('drops known, duplicate, untyped and empty entries', () => {
    const reply = JSON.stringify([
      { type: 'decision', text: '  use <=  ON expiry ' },
      { type: 'open', text: 'not a distill type' },
      { type: 'finding', text: '' },
      { type: 'finding', text: 'A' },
      { type: 'finding', text: 'a' },
      'junk',
    ])
    expect(parseDistill(reply, known)).toEqual([{ type: 'finding', text: 'A' }])
  })

  test('caps the count and the length', () => {
    const many = Array.from({ length: 20 }, (_, i) => ({ type: 'finding', text: `f${i} ${'x'.repeat(600)}` }))
    const out = parseDistill(JSON.stringify(many), [])
    expect(out.length).toBe(8)
    expect(out[0]?.text.length).toBe(500)
  })

  test('no array, no events', () => {
    expect(parseDistill('Nothing to add.', known)).toEqual([])
    expect(parseDistill('[not json', known)).toEqual([])
  })
})

describe('transcriptExcerpt', () => {
  const messages = [
    { role: 'user', text: 'first question' },
    { role: 'assistant', text: '' },
    { role: 'assistant', text: 'first answer' },
    { role: 'user', text: 'second question' },
  ]

  test('keeps the turns in order and skips empty ones', () => {
    expect(transcriptExcerpt(messages, 1000)).toBe('user: first question\nassistant: first answer\nuser: second question')
  })

  test('keeps the newest turns when it has to cut', () => {
    expect(transcriptExcerpt(messages, 50)).toBe('assistant: first answer\nuser: second question')
  })

  test('cuts a single huge turn from its start, keeping its end', () => {
    const out = transcriptExcerpt([{ role: 'user', text: 'x'.repeat(100) + 'END' }], 20)
    expect(out.length).toBe(20)
    expect(out.endsWith('END')).toBe(true)
  })
})
